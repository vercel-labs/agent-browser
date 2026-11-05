//! Saved flows: record a task once in the browser, then replay its
//! first-party API requests directly over HTTP.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::actions::HarEntry;

mod hint;
mod hosts;
pub mod http;
#[cfg(test)]
mod memory;
mod model;
mod outcome;
pub mod page;
mod recorder;
pub mod replay;
mod send;
mod template;

pub use hint::*;
pub use hosts::*;
pub use http::*;
pub use model::*;
pub use outcome::*;
pub use recorder::*;
pub use replay::*;
pub use send::*;
pub use template::{Body, Parts, Slot};

// ---------------------------------------------------------------------------
// Request selection
// ---------------------------------------------------------------------------

const TELEMETRY_PATH_MARKERS: &[&str] = &[
    "/collect",
    "/track",
    "/beacon",
    "/analytics",
    "/pixel",
    "/telemetry",
    "/metrics",
    "/log",
    "/rum",
    "/sentry",
];

/// How a recorded request header is treated.
#[derive(PartialEq)]
enum HeaderClass {
    /// Carries a credential the session supplies at run time. Never saved; a
    /// request recorded with one may get a 401 or 403 over HTTP.
    Credential,
    /// Per-connection or session-owned. Never saved.
    Dropped,
    /// Set by the browser itself; saved, but not sent by an in-page fetch.
    BrowserSet,
    Kept,
}

fn classify_header(name: &str) -> HeaderClass {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "authorization" | "proxy-authorization" => HeaderClass::Credential,
        _ if lower.contains("token") || lower.contains("csrf") => HeaderClass::Credential,
        "cookie" | "host" | "content-length" | "connection" | "accept-encoding" => {
            HeaderClass::Dropped
        }
        "user-agent" | "referer" | "origin" | "accept-language" | "priority" | "dnt" => {
            HeaderClass::BrowserSet
        }
        _ if lower.starts_with("sec-") => HeaderClass::BrowserSet,
        // Per-request tracing and client identity: not part of what a flow replays.
        "x-client-ip" | "x-forwarded-for" | "x-real-ip" | "forwarded" | "traceparent"
        | "tracestate" | "x-request-id" | "request-id" | "x-amzn-trace-id" | "sentry-trace"
        | "baggage" | "cf-connecting-ip" | "true-client-ip" => HeaderClass::Dropped,
        _ => HeaderClass::Kept,
    }
}

fn is_telemetry(url: &str) -> bool {
    let path = url::Url::parse(url)
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_default();
    TELEMETRY_PATH_MARKERS
        .iter()
        .any(|m| path == *m || path.starts_with(&format!("{}/", m)) || path.ends_with(m))
}

fn is_api_entry(e: &HarEntry, policy: &HostPolicy) -> bool {
    matches!(e.resource_type.as_str(), "XHR" | "Fetch")
        && e.mime_type.contains("json")
        && e.status.map(|s| (200..300).contains(&s)).unwrap_or(false)
        && policy.allows_url(&e.url)
        && !is_telemetry(&e.url)
}

// ---------------------------------------------------------------------------
// Response signatures
// ---------------------------------------------------------------------------

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

const SIGNATURE_DEPTH: usize = 3;

/// Shape of a JSON body, a few levels deep: object keys with their types,
/// and the element shape for arrays.
pub fn signature(v: &Value) -> Value {
    signature_at(v, SIGNATURE_DEPTH)
}

fn signature_at(v: &Value, depth: usize) -> Value {
    match v {
        Value::Object(map) if depth > 0 => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sig = Map::new();
            for k in keys {
                sig.insert(k.clone(), signature_at(&map[k], depth - 1));
            }
            json!({ "object": sig })
        }
        Value::Array(items) if depth > 0 => match items.first() {
            Some(first) => json!({ "array": signature_at(first, depth - 1) }),
            None => json!({ "array": null }),
        },
        other => json!({ "scalar": type_name(other) }),
    }
}

fn signature_type(sig: &Value) -> &str {
    if sig.get("object").is_some() {
        "object"
    } else if sig.get("array").is_some() {
        "array"
    } else {
        sig.get("scalar")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
    }
}

/// Every key the recording saw must still be there with the same type. New
/// keys are fine; a missing or retyped key means the contract moved.
pub fn signature_matches(recorded: &Value, current: &Value) -> Result<(), String> {
    matches_at(recorded, current, "", false)
}

/// Like `signature_matches`, but a recorded count may be missing: many APIs
/// drop the totals when a list is empty. A missing list is never accepted.
fn matches_without_counts(recorded: &Value, current: &Value) -> bool {
    matches_at(recorded, current, "", true).is_ok()
}

/// A non-empty list in `body` under a key the recording did not have, to name
/// as where a missing recorded list went. A list whose elements are the same
/// type as the missing one's comes first.
pub(crate) fn moved_list(recorded: &Value, body: &Value) -> Option<String> {
    let current = signature_at(body, 8);
    let mut lost = Vec::new();
    missing_lists(recorded, &current, "", &mut lost);
    let mut found = Vec::new();
    appeared_lists(recorded, &current, "", &mut found);
    let same_type = |elem: &Value| {
        lost.iter()
            .any(|l| signature_type(l) == signature_type(elem))
    };
    found
        .iter()
        .find(|(_, elem)| same_type(elem))
        .or(found.first())
        .map(|(p, _)| p.clone())
}

/// Element shapes of the recorded lists that `current` no longer has.
fn missing_lists(recorded: &Value, current: &Value, path: &str, out: &mut Vec<Value>) {
    let Some(rec) = recorded.get("object").and_then(|v| v.as_object()) else {
        return;
    };
    let cur = current.get("object").and_then(|v| v.as_object());
    for (key, sig) in rec {
        let child = if path.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", path, key)
        };
        match cur.and_then(|c| c.get(key)) {
            Some(cur_sig) => missing_lists(sig, cur_sig, &child, out),
            None => lists_within(sig, &child, &mut |_, elem| out.push(elem.clone())),
        }
    }
}

/// Non-empty lists in `current` under keys the recording did not have, with their element shape.
fn appeared_lists(recorded: &Value, current: &Value, path: &str, out: &mut Vec<(String, Value)>) {
    let Some(cur) = current.get("object").and_then(|v| v.as_object()) else {
        return;
    };
    let rec = recorded.get("object").and_then(|v| v.as_object());
    for (key, sig) in cur {
        let child = if path.is_empty() {
            key.clone()
        } else {
            format!("{}.{}", path, key)
        };
        match rec.and_then(|r| r.get(key)) {
            None => lists_within(sig, &child, &mut |p, elem| {
                if !elem.is_null() {
                    out.push((p.to_string(), elem.clone()))
                }
            }),
            Some(rec_sig) => appeared_lists(rec_sig, sig, &child, out),
        }
    }
}

/// Calls `f` with the path and element shape of each list at or under `sig`.
/// A recorded list that was cut off by the signature depth has no element shape.
fn lists_within(sig: &Value, path: &str, f: &mut dyn FnMut(&str, &Value)) {
    if signature_type(sig) == "array" {
        f(path, sig.get("array").unwrap_or(&Value::Null));
    } else if let Some(map) = sig.get("object").and_then(|v| v.as_object()) {
        for (k, v) in map {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", path, k)
            };
            lists_within(v, &child, f);
        }
    }
}

fn matches_at(
    recorded: &Value,
    current: &Value,
    path: &str,
    counts_optional: bool,
) -> Result<(), String> {
    let (rec_ty, cur_ty) = (signature_type(recorded), signature_type(current));
    if rec_ty == "null" || cur_ty == "null" {
        return Ok(());
    }
    if rec_ty != cur_ty {
        let at = if path.is_empty() {
            "response".to_string()
        } else {
            format!("'{}'", path)
        };
        return Err(format!("{} changed from {} to {}", at, rec_ty, cur_ty));
    }
    if let (Some(rec), Some(cur)) = (
        recorded.get("object").and_then(|v| v.as_object()),
        current.get("object").and_then(|v| v.as_object()),
    ) {
        for (key, sig) in rec {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path, key)
            };
            match cur.get(key) {
                // Counts and totals often go with an empty list.
                None if counts_optional && signature_type(sig) == "number" => {}
                None => return Err(format!("key '{}' is missing", child)),
                Some(cur_sig) => matches_at(sig, cur_sig, &child, counts_optional)?,
            }
        }
    }
    if let (Some(rec), Some(cur)) = (recorded.get("array"), current.get("array")) {
        if !rec.is_null() && !cur.is_null() {
            matches_at(rec, cur, &format!("{}[]", path), counts_optional)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

pub fn parse_params(cmd: &Value) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    if let Some(obj) = cmd.get("params").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            let value = v
                .as_str()
                .ok_or(format!("param '{}' must be a string", k))?;
            out.push((k.clone(), value.to_string()));
        }
    }
    Ok(out)
}

fn map_strings(v: &mut Value, f: &dyn Fn(&str) -> Result<String, String>) -> Result<(), String> {
    match v {
        Value::String(s) => *s = f(s)?,
        Value::Array(items) => {
            for item in items {
                map_strings(item, f)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                map_strings(item, f)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

pub struct SaveInput<'a> {
    pub name: &'a str,
    pub recorder: &'a FlowRecorder,
    /// The recorded steps this flow covers.
    pub commands: &'a [(u64, Value)],
    pub description: Option<&'a str>,
    pub entries: &'a [HarEntry],
    pub params: &'a [(String, String)],
    pub output: Option<&'a str>,
    pub created_at: String,
    /// The page shows a password field at save time.
    pub login_wall: bool,
}

fn start_url(commands: &[(u64, Value)]) -> Option<String> {
    commands.iter().find_map(|(_, c)| {
        (c.get("action").and_then(|a| a.as_str()) == Some("navigate"))
            .then(|| c.get("url").and_then(|u| u.as_str()).map(String::from))
            .flatten()
    })
}

pub fn describe_command(cmd: &Value) -> String {
    let action = cmd.get("action").and_then(|a| a.as_str()).unwrap_or("?");
    let target = ["url", "selector", "name", "text", "value"]
        .iter()
        .find_map(|k| cmd.get(*k).and_then(|v| v.as_str()));
    match target {
        Some(t) => format!("{} {}", action, t),
        None => action.to_string(),
    }
}

/// How a dropped request is named: its path without the query, with any
/// segment that holds a secret hidden. When the secret is unknown, only the host.
fn dropped_name(e: &HarEntry, secrets: &[String], known: bool) -> String {
    let Ok(url) = url::Url::parse(&e.url) else {
        return format!("{} ?", e.method);
    };
    if !known {
        return format!("{} {}", e.method, url.host_str().unwrap_or("?"));
    }
    let path: Vec<String> = url
        .path()
        .split('/')
        .map(|seg| {
            let decoded = urlencoding::decode(seg).map(|d| d.into_owned());
            let hit = secrets.iter().any(|s| {
                let escaped = serde_json::to_string(s).unwrap_or_default();
                seg.contains(s.as_str())
                    || seg.contains(urlencoding::encode(s).as_ref())
                    || seg.contains(escaped.trim_matches('"'))
                    || decoded.as_ref().is_ok_and(|d| d.contains(s.as_str()))
            });
            if hit { "<redacted>" } else { seg }.to_string()
        })
        .collect();
    let path = path.join("/");
    if holds_secret(&path, secrets) {
        return format!("{} {}", e.method, url.host_str().unwrap_or("?"));
    }
    format!("{} {}", e.method, path)
}

fn carries_secret(e: &HarEntry, secrets: &[String]) -> bool {
    holds_secret(&e.url, secrets)
        || e.post_data
            .as_deref()
            .is_some_and(|t| holds_secret(t, secrets))
        || e.request_headers
            .iter()
            .any(|(_, v)| holds_secret(v, secrets))
}

/// Whether `text` holds a secret as is, form-encoded, percent-encoded or JSON-escaped.
fn holds_secret(text: &str, secrets: &[String]) -> bool {
    secrets.iter().any(|secret| {
        let encoded = urlencoding::encode(secret);
        let escaped = serde_json::to_string(secret).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        text.contains(secret.as_str())
            || text.contains(encoded.as_ref())
            || text.contains(escaped)
            || decoded_forms(text)
                .iter()
                .any(|d| d.contains(secret.as_str()))
    })
}

/// A request text as the server reads it: form encoding (`+` is a space),
/// percent encoding, and the strings inside a JSON body, whose escapes the
/// parser resolves.
fn decoded_forms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(plus) = urlencoding::decode(&text.replace('+', " ")) {
        out.push(plus.into_owned());
    }
    if let Ok(pct) = urlencoding::decode(text) {
        out.push(pct.into_owned());
    }
    if let Ok(json) = serde_json::from_str::<Value>(text) {
        let mut strings = Vec::new();
        collect_scalars(&json, &mut strings);
        out.extend(strings);
    }
    out
}

/// Data a recorded request can answer with: the JSON body of an API call,
/// or each JSON block embedded in a first-party HTML document (Next.js
/// `__NEXT_DATA__`, JSON-LD, framework state), for sites that render on the
/// server.
fn candidates<'a>(
    tagged: &[&'a HarEntry],
    policy: &HostPolicy,
) -> Vec<(&'a HarEntry, Value, Option<Value>)> {
    let mut out = Vec::new();
    for e in tagged.iter().copied() {
        let Some(body) = e
            .response_body
            .as_deref()
            .filter(|_| !e.response_body_base64)
        else {
            continue;
        };
        if is_api_entry(e, policy) {
            if let Ok(parsed) = serde_json::from_str::<Value>(body) {
                out.push((e, parsed, None));
            }
        } else if is_document_entry(e, policy) {
            for (spec, parsed) in embedded_json(body) {
                out.push((e, parsed, Some(spec)));
            }
        }
    }
    out
}

fn is_document_entry(e: &HarEntry, policy: &HostPolicy) -> bool {
    e.resource_type == "Document"
        && e.method == "GET"
        && e.mime_type.contains("html")
        && e.status.map(|s| (200..300).contains(&s)).unwrap_or(false)
        && policy.allows_url(&e.url)
}

/// `<script>` blocks whose whole content is a JSON object or array, each with
/// a spec that finds it again: its `id` if it has one, else its position.
pub fn embedded_json(html: &str) -> Vec<(Value, Value)> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut pos = 0;
    let mut index = 0;
    while let Some(start) = lower[pos..].find("<script") {
        let open = pos + start;
        let Some(tag_end) = lower[open..].find('>').map(|i| open + i) else {
            break;
        };
        let Some(close) = lower[tag_end..].find("</script").map(|i| tag_end + i) else {
            break;
        };
        let attrs = &html[open + 7..tag_end];
        let content = html[tag_end + 1..close].trim();
        pos = close + 8;
        let ty = attr(attrs, "type").unwrap_or_default().to_ascii_lowercase();
        if !(ty.is_empty() || ty.contains("json")) {
            continue;
        }
        let content = assigned_value(content).unwrap_or(content);
        if !(content.starts_with('{') || content.starts_with('[')) {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(content) else {
            continue;
        };
        let spec = match attr(attrs, "id") {
            Some(id) => json!({ "scriptId": id }),
            None => json!({ "scriptIndex": index }),
        };
        index += 1;
        out.push((spec, parsed));
    }
    out
}

/// The JSON in a script that only assigns it: `window.__context__ = {...};`.
fn assigned_value(content: &str) -> Option<&str> {
    let eq = content.find('=')?;
    let target = content[..eq].trim();
    let target = ["window.", "self.", "var ", "let ", "const "]
        .iter()
        .find_map(|p| target.strip_prefix(p))
        .unwrap_or(target)
        .trim();
    let simple = !target.is_empty()
        && target.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | '[' | ']' | '"' | '\'')
        });
    simple.then(|| content[eq + 1..].trim().trim_end_matches(';').trim())
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let at = lower.find(&format!("{}=", name))?;
    let rest = &attrs[at + name.len() + 1..];
    let (quote, rest) = match rest.chars().next()? {
        q @ ('"' | '\'') => (q, &rest[1..]),
        _ => (' ', rest),
    };
    let end = rest
        .find(|c: char| c == quote || (quote == ' ' && c == '>'))
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Pull the recorded JSON block back out of a document body.
pub fn extract_embedded(html: &str, spec: &Value) -> Option<String> {
    embedded_json(html)
        .into_iter()
        .find(|(s, _)| {
            (spec.get("scriptId").is_some() && s.get("scriptId") == spec.get("scriptId"))
                || (spec.get("scriptIndex").is_some()
                    && s.get("scriptIndex") == spec.get("scriptIndex"))
        })
        .map(|(_, v)| v.to_string())
}

/// Params the agent passed, plus query values of the start URL that the
/// recorded requests sent too (`?q=tea` -> param `q`), including the page
/// itself when its HTML embeds the data.
fn with_page_query_params(
    start: &str,
    policy: &HostPolicy,
    given: &[(String, String)],
    entries: &[HarEntry],
) -> Vec<(String, String)> {
    let mut out = given.to_vec();
    let Ok(url) = url::Url::parse(start) else {
        return out;
    };
    for (key, value) in url.query_pairs() {
        let (key, value) = (key.into_owned(), value.into_owned());
        if value.chars().count() < 3 || out.iter().any(|(k, v)| *k == key || *v == value) {
            continue;
        }
        let encoded = urlencoding::encode(&value).into_owned();
        let sent = entries.iter().any(|e| {
            (is_api_entry(e, policy)
                || (is_document_entry(e, policy)
                    && e.response_body
                        .as_deref()
                        .is_some_and(|b| !embedded_json(b).is_empty())))
                && (e.url.contains(&encoded)
                    || e.post_data.as_deref().is_some_and(|b| b.contains(&value)))
        });
        if sent {
            out.push((key, value));
        }
    }
    out
}

/// Build the flow file and the save report from what was recorded.
pub fn build_flow(input: SaveInput<'_>) -> Result<(Flow, Value), String> {
    let start = start_url(input.commands)
        .or_else(|| {
            input
                .commands
                .first()
                .and_then(|(seq, _)| input.recorder.meta.get(seq))
                .and_then(|m| m.from_url.clone())
                .filter(|u| u.starts_with("http"))
        })
        .or_else(|| {
            input
                .recorder
                .page_url
                .clone()
                .filter(|u| u.starts_with("http"))
        })
        .ok_or("nothing to save: the flow has no `open <url>` command and no page was open")?;
    let host = host_of(&start).ok_or("the flow's start URL has no host")?;
    let base = base_domain(&host);
    let policy = HostPolicy::from_start(&start);
    // Values typed into ordinary fields are params: the step keeps a
    // placeholder, and a `--param` with the same value names it.
    let mut typed: Vec<(String, String)> = Vec::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    for (name, value, seq) in &input.recorder.typed {
        if !input.commands.iter().any(|(s, _)| s == seq) {
            continue;
        }
        let mut name = name.clone();
        if let Some((explicit, _)) = input.params.iter().find(|(_, v)| v == value) {
            if *explicit != name && !renames.iter().any(|(from, _)| *from == name) {
                renames.push((name, explicit.clone()));
            }
            continue;
        }
        if input.params.iter().any(|(n, _)| *n == name) {
            let renamed = format!("{}_typed", name);
            renames.push((name, renamed.clone()));
            name = renamed;
        }
        if !typed.iter().any(|(n, _)| *n == name) {
            typed.push((name, value.clone()));
        }
    }
    let commands: Vec<(u64, Value)> = input
        .commands
        .iter()
        .map(|(seq, cmd)| {
            let mut cmd = cmd.clone();
            let _ = map_strings(&mut cmd, &|text| {
                let mut text = text.to_string();
                for (from, to) in &renames {
                    text = text.replace(&format!("{{{{{}}}}}", from), &format!("{{{{{}}}}}", to));
                }
                Ok(text)
            });
            (*seq, cmd)
        })
        .collect();
    let mut explicit_and_typed = input.params.to_vec();
    explicit_and_typed.extend(
        typed
            .iter()
            .filter(|(_, v)| v.chars().count() >= 3)
            .cloned(),
    );
    let mut given = with_page_query_params(&start, &policy, &explicit_and_typed, input.entries);

    let mut steps: Vec<Step> = Vec::new();
    let mut gaps = Vec::new();
    let mut dropped_headers: Vec<String> = Vec::new();
    let mut request_ids: Vec<String> = Vec::new();
    let mut bodies: Vec<(String, Value)> = Vec::new();
    let mut secret_requests: Vec<String> = Vec::new();
    let mut secret_dropped: Vec<String> = Vec::new();
    let mut har_ids: HashMap<String, String> = HashMap::new();
    // The id each request had in the capture (`s<seq>r<n>`), which `flow
    // steps` numbering gives, next to its id in the saved range.
    let mut capture_ids: Vec<(String, String)> = Vec::new();
    // Recorded Referer values, to restore if a page-query param turns out to
    // be carried by the Referer alone.
    let mut referers: Vec<(String, String, String)> = Vec::new();

    for (index, (seq, cmd)) in commands.iter().enumerate() {
        let tagged: Vec<&HarEntry> = input
            .entries
            .iter()
            .filter(|e| input.recorder.request_tags.get(&e.request_id) == Some(seq))
            .collect();
        let mut requests: Vec<Request> = Vec::new();
        let mut seen = Vec::new();
        for (e, parsed, extract) in candidates(&tagged, &policy) {
            let known = carries_secret(e, &input.recorder.secrets);
            if known || input.recorder.unknown_secret_seqs.contains(seq) {
                if !secret_requests.contains(&e.request_id) {
                    secret_requests.push(e.request_id.clone());
                    let what = dropped_name(e, &input.recorder.secrets, known);
                    let verb = if known { "carried" } else { "may carry" };
                    secret_dropped.push(format!("{} {}", what, verb));
                    gaps.push(json!({
                        "step": index + 1,
                        "command": describe_command(cmd),
                        "reason": format!(
                            "{} {} a value typed or pasted into a secret field, so it was not saved; fill a non-secret field or use a declared tool for a reusable flow",
                            what, verb
                        ),
                    }));
                }
                continue;
            }
            let key = format!(
                "{} {} {} {}",
                e.method,
                e.url,
                e.post_data.as_deref().unwrap_or(""),
                extract.as_ref().map(|x| x.to_string()).unwrap_or_default()
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let body_len = serde_json::to_string(&parsed).map(|t| t.len()).unwrap_or(0);
            let mut headers = BTreeMap::new();
            let mut stripped_auth = false;
            for (k, v) in &e.request_headers {
                let lower = k.to_ascii_lowercase();
                if lower.starts_with(':') {
                    continue;
                }
                let class = classify_header(&lower);
                if matches!(class, HeaderClass::Credential | HeaderClass::Dropped) {
                    if class == HeaderClass::Credential {
                        stripped_auth = true;
                    }
                    if !dropped_headers.contains(&lower) && lower != "host" {
                        dropped_headers.push(lower);
                    }
                    continue;
                }
                if lower == "referer" {
                    referers.push((
                        format!("s{}r{}", index + 1, requests.len() + 1),
                        k.clone(),
                        v.clone(),
                    ));
                }
                headers.insert(k.clone(), template::templatize(v, &given, Slot::Raw));
            }
            let item_fields = item_fields(&parsed);
            let id = format!("s{}r{}", index + 1, requests.len() + 1);
            // A write's response is not a source: the flow would have to send it.
            if !is_write(&e.method, e.post_data.as_deref()) {
                bodies.push((id.clone(), parsed.clone()));
            }
            request_ids.push(id.clone());
            capture_ids.push((format!("s{}r{}", seq, requests.len() + 1), id.clone()));
            har_ids.insert(id.clone(), e.request_id.clone());
            requests.push(Request {
                id,
                method: e.method.clone(),
                url: template::templatize(&e.url, &given, Slot::Url),
                headers,
                body: e
                    .post_data
                    .as_deref()
                    .map(|b| template::templatize_body(b, &given, &e.request_headers)),
                tier: None,
                expect: Expect {
                    status: e.status.map(|s| s as u16),
                    mime: e.mime_type.clone(),
                    signature: signature(&parsed),
                    bytes: body_len as u64,
                    item_fields,
                    extract,
                    // A key with a dot in it cannot be told from a nested path.
                    list: main_list(&parsed)
                        .and_then(|(path, _)| path)
                        .filter(|path| !list_gone(path, &parsed)),
                    stripped_auth,
                },
            });
        }
        let action = cmd.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let interactive = matches!(
            action,
            "navigate"
                | "click"
                | "dblclick"
                | "fill"
                | "type"
                | "press"
                | "select"
                | "check"
                | "uncheck"
                | "submit"
                | "getbyrole"
        );
        if requests.is_empty() && interactive {
            let reason = if input.login_wall && index + 1 == input.commands.len() {
                "the page asked for a login; this step needs an account, and the flow stops before it"
            } else if tagged.is_empty() {
                "no network traffic while this command ran"
            } else if tagged.iter().any(|e| policy.allows_url(&e.url)) {
                "first-party traffic was documents or static files, not a JSON API"
            } else {
                "only third-party traffic"
            };
            gaps.push(
                json!({ "step": index + 1, "command": describe_command(cmd), "reason": reason }),
            );
        }
        steps.push(Step {
            seq: *seq,
            ok: input.recorder.is_ok(*seq),
            command: cmd.clone(),
            requests,
        });
    }

    if request_ids.is_empty() && !secret_dropped.is_empty() {
        return Err(format!(
            "no first-party JSON requests could be saved for {}: {} a value typed into a secret field, so not saved. To make a reusable flow, fill a non-secret field or use a declared tool",
            base,
            secret_dropped.join(" and ")
        ));
    }
    if request_ids.is_empty() {
        return Err(format!(
            "no first-party JSON requests were recorded for {}; this flow has nothing to replay over HTTP",
            base
        ));
    }
    // A page-query value that only a Referer carries is the page's address,
    // not an input of the requests: keep the recorded Referer.
    let unused: Vec<String> = given
        .iter()
        .filter(|(k, _)| {
            !input.params.iter().any(|(n, _)| n == k) && !typed.iter().any(|(n, _)| n == k)
        })
        .map(|(k, _)| k.clone())
        .filter(|k| {
            let placeholder = format!("{{{{{}}}}}", k);
            let used = steps.iter().flat_map(|s| s.requests.iter()).any(|r| {
                r.url.contains(&placeholder)
                    || r.body.as_deref().is_some_and(|b| b.contains(&placeholder))
                    || r.headers.iter().any(|(h, v)| {
                        !h.eq_ignore_ascii_case("referer") && v.contains(&placeholder)
                    })
            });
            !used
        })
        .collect();
    if !unused.is_empty() {
        given.retain(|(k, _)| !unused.contains(k));
        for req in steps.iter_mut().flat_map(|s| s.requests.iter_mut()) {
            for (id, key, original) in &referers {
                if *id == req.id {
                    let kept = template::templatize(original, &given, Slot::Raw);
                    req.headers.insert(key.clone(), kept);
                }
            }
        }
    }
    let page_segments: Vec<String> = url::Url::parse(&start)
        .ok()
        .and_then(|u| u.path_segments().map(|p| p.map(String::from).collect()))
        .unwrap_or_default();
    let links = link_requests(&mut steps, &bodies, &har_ids, &page_segments, &given);
    let mut params: Vec<Param> = given
        .iter()
        .map(|(k, v)| {
            if input.params.iter().any(|(n, _)| n == k) {
                Param {
                    name: k.clone(),
                    example: Some(v.clone()),
                    source: ParamSource::Typed,
                    from: None,
                }
            } else if typed.iter().any(|(n, _)| n == k) {
                Param {
                    name: k.clone(),
                    example: None,
                    source: ParamSource::Typed,
                    from: None,
                }
            } else {
                Param {
                    name: k.clone(),
                    example: Some(v.clone()),
                    source: ParamSource::Query,
                    from: Some(ParamFrom {
                        page_query: Some(k.clone()),
                        ..ParamFrom::default()
                    }),
                }
            }
        })
        .collect();
    for (name, _) in &typed {
        if !given.iter().any(|(n, _)| n == name) {
            params.push(Param {
                name: name.clone(),
                example: None,
                source: ParamSource::Typed,
                from: None,
            });
        }
    }
    params.extend(links.params);
    let output = match input.output {
        Some(id) if request_ids.iter().any(|r| r == id) => {
            let url_of = |saved: &str| {
                steps
                    .iter()
                    .flat_map(|s| s.requests.iter())
                    .find(|r| r.id == saved)
                    .map(|r| r.url.clone())
                    .unwrap_or_default()
            };
            match capture_ids.iter().find(|(c, saved)| c == id && saved != id) {
                Some((_, other)) => {
                    return Err(format!(
                        "{id} is ambiguous: saved {id} ({}) or capture {id}, saved as {other} ({}); \
                         use --output with one of them after renumbering the range, or save from step 1",
                        url_of(id),
                        url_of(other)
                    ));
                }
                None => id.to_string(),
            }
        }
        Some(id) => match capture_ids.iter().find(|(c, _)| c == id) {
            Some((_, saved)) => saved.clone(),
            None => {
                let mut valid: Vec<String> = request_ids.clone();
                for (c, s) in &capture_ids {
                    if c != s {
                        valid.push(c.clone());
                    }
                }
                return Err(format!(
                    "unknown request id '{}' for --output; valid ids: {}",
                    id,
                    valid.join(", ")
                ));
            }
        },
        None => default_output(&steps),
    };

    let item_fields = steps
        .iter()
        .flat_map(|s| s.requests.iter())
        .find(|r| r.id == output)
        .map(|r| r.expect.item_fields.clone())
        .unwrap_or_default();
    let flow = Flow {
        version: FLOW_VERSION,
        name: input.name.to_string(),
        origin: host.clone(),
        start_url: start.clone(),
        allowed_hosts: policy.hosts().to_vec(),
        created_at: input.created_at.clone(),
        created_by: Some(CreatedBy {
            tool: "agent-browser".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }),
        description: input.description.map(String::from),
        redacted_steps: commands
            .iter()
            .enumerate()
            .filter(|(_, (_, c))| c["redacted"] == true)
            .map(|(i, _)| i + 1)
            .collect(),
        params,
        output: output.clone(),
        item_fields,
        provenance: "observed".to_string(),
        verified_at: None,
        writes: collect_writes(&input, &policy, &given, &har_ids),
        steps,
    };

    let requests_report: Vec<Value> = flow
        .requests()
        .map(|r| {
            json!({
                "id": r.id,
                "method": r.method,
                "url": r.url,
                "bytes": r.expect.bytes,
            })
        })
        .collect();

    let report = json!({
        "name": input.name,
        "origin": host,
        "steps": input.commands.len(),
        "requests": requests_report,
        "output": output,
        "params": flow.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        "detectedParams": flow.params.iter().filter(|p| p.from.is_some()).collect::<Vec<_>>(),
        "references": links.references,
        "writes": flow.writes,
        "gaps": gaps,
        "droppedHeaders": dropped_headers,
        "verified": false,
    });
    Ok((flow, report))
}

// ---------------------------------------------------------------------------
// Params and references found in the recording
// ---------------------------------------------------------------------------

struct Links {
    /// Values that came from an item of an earlier list: the agent picks one.
    params: Vec<Param>,
    /// Values that came from a single field of an earlier response: filled in
    /// at run time from that response.
    references: Vec<Value>,
}

/// Values a request sends: query values, path segments and JSON body scalars.
fn sent_values(req: &Request) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(url) = url::Url::parse(&req.url) {
        out.extend(url.query_pairs().map(|(_, v)| v.into_owned()));
        if let Some(segs) = url.path_segments() {
            out.extend(segs.map(|s| {
                urlencoding::decode(s)
                    .map(|d| d.into_owned())
                    .unwrap_or_default()
            }));
        }
    }
    if let Some(body) = req
        .body
        .as_deref()
        .and_then(|b| serde_json::from_str::<Value>(b).ok())
    {
        collect_scalars(&body, &mut out);
    }
    // Short values (ids like "1", flags like "es") match unrelated fields by chance.
    out.retain(|v| v.chars().count() >= 3 && !v.contains("{{"));
    out.sort();
    out.dedup();
    out
}

fn collect_scalars(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => m.values().for_each(|c| collect_scalars(c, out)),
        Value::Array(a) => a.iter().for_each(|c| collect_scalars(c, out)),
        Value::String(s) => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        _ => {}
    }
}

fn looks_generated(v: &str) -> bool {
    let mixed_case =
        v.chars().any(|c| c.is_ascii_uppercase()) && v.chars().any(|c| c.is_ascii_lowercase());
    v.chars().count() >= 6 && (v.chars().any(|c| c.is_ascii_digit()) || mixed_case)
}

/// Replace a value, plain or URL-encoded, with a placeholder.
fn replace_value(text: &str, value: &str, placeholder: &str) -> String {
    let encoded = urlencoding::encode(value).into_owned();
    replace_token(
        &replace_token(text, &encoded, placeholder),
        value,
        placeholder,
    )
}

/// Replace `needle` only where it stands alone, not inside a longer word:
/// `401` in `theater=401`, never in `14011`.
fn replace_token(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(needle) {
        let before = rest[..i].chars().next_back();
        let after = rest[i + needle.len()..].chars().next();
        let bounded = !before.is_some_and(word) && !after.is_some_and(word);
        out.push_str(&rest[..i]);
        out.push_str(if bounded { with } else { needle });
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    out
}

/// Where `needle` sits in `body`, as a path like `token` or `data[].sessionId`,
/// and whether the path crosses a list.
fn find_value(body: &Value, needle: &str, path: &str) -> Option<(String, bool)> {
    match body {
        Value::Object(m) => m.iter().find_map(|(k, v)| {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{}.{}", path, k)
            };
            find_value(v, needle, &child)
        }),
        Value::Array(a) => a
            .iter()
            .find_map(|v| find_value(v, needle, &format!("{}[]", path)))
            .map(|(p, _)| (p, true)),
        Value::String(s) if s == needle => Some((path.to_string(), false)),
        Value::Number(n) if n.to_string() == needle => Some((path.to_string(), false)),
        _ => None,
    }
}

fn link_requests(
    steps: &mut [Step],
    bodies: &[(String, Value)],
    har_ids: &HashMap<String, String>,
    page_segments: &[String],
    given: &[(String, String)],
) -> Links {
    let mut links = Links {
        params: Vec::new(),
        references: Vec::new(),
    };
    let given_values: Vec<&str> = given.iter().map(|(_, v)| v.as_str()).collect();
    // Path segments of each request, to tell a response value that is the
    // site's own structure (`api`, `v10`) from one the run chooses.
    let segments: HashMap<String, Vec<String>> = steps
        .iter()
        .flat_map(|s| s.requests.iter())
        .map(|r| {
            let segs = url::Url::parse(&r.url)
                .ok()
                .and_then(|u| u.path_segments().map(|p| p.map(String::from).collect()))
                .unwrap_or_default();
            (r.id.clone(), segs)
        })
        .collect();
    // Position of every request, writes included, so a write can take a value
    // from any response that came before it.
    let rank: HashMap<String, usize> = steps
        .iter()
        .flat_map(|s| s.requests.iter())
        .enumerate()
        .map(|(i, r)| (r.id.clone(), i))
        .collect();
    for step in steps.iter_mut() {
        for req in step.requests.iter_mut() {
            let id = req.id.clone();
            let own = rank.get(&id).copied().unwrap_or(0);
            let earlier: Vec<(String, Value)> = bodies
                .iter()
                .filter(|(from, _)| rank.get(from).is_some_and(|r| *r < own))
                .cloned()
                .collect();
            for value in sent_values(req) {
                // The page's own path (owner, repo, section) is the site's
                // structure, not a choice the run makes.
                if given_values.contains(&value.as_str()) || page_segments.contains(&value) {
                    continue;
                }
                // Blocks of the same document are one response, not a chain.
                let same_response = |from: &str| {
                    har_ids.get(from).is_some() && har_ids.get(from) == har_ids.get(&id)
                };
                // The latest response that holds the value supplies it, except
                // an echo: a response whose own request path already has the
                // value just repeats it. A list is preferred over a single field.
                let sources: Vec<(String, String, bool)> = earlier
                    .iter()
                    .rev()
                    .filter(|(from, _)| !same_response(from))
                    .filter(|(from, _)| {
                        !segments.get(from).is_some_and(|segs| segs.contains(&value))
                    })
                    .filter_map(|(from, body)| {
                        find_value(body, &value, "").map(|(p, l)| (from.clone(), p, l))
                    })
                    .collect();
                let Some((from, path, in_list)) = sources
                    .iter()
                    .find(|(_, _, l)| *l)
                    .or(sources.first())
                    .cloned()
                else {
                    continue;
                };
                // A value in the path of a request sent before that response
                // could have supplied it, or of the request it came from, is
                // structure rather than data from that response.
                let own_rank = rank.get(&from).copied().unwrap_or(0);
                let structural = segments.iter().any(|(rid, segs)| {
                    rank.get(rid).is_some_and(|r| *r <= own_rank) && segs.contains(&value)
                });
                if structural {
                    continue;
                }
                // Words and codes like "es-AR" or "public" repeat across a site's
                // responses; only generated values (ids, keys) are worth chaining.
                if !in_list && !looks_generated(&value) {
                    continue;
                }
                let placeholder = if in_list {
                    let field = path
                        .rsplit(['.', ']'])
                        .next()
                        .map(|f| {
                            f.chars()
                                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                                .collect::<String>()
                        })
                        .filter(|f| !f.is_empty())
                        .unwrap_or_else(|| "value".to_string());
                    let name = match links
                        .params
                        .iter()
                        .find(|p| p.example.as_deref() == Some(value.as_str()))
                    {
                        Some(p) => p.name.clone(),
                        None => {
                            let mut name = field.clone();
                            let mut n = 2;
                            while links.params.iter().any(|p| p.name == name)
                                || given.iter().any(|(k, _)| *k == name)
                            {
                                name = format!("{}{}", field, n);
                                n += 1;
                            }
                            links.params.push(Param {
                                name: name.clone(),
                                example: Some(value.clone()),
                                source: ParamSource::Response,
                                from: Some(ParamFrom {
                                    request: Some(from.clone()),
                                    list: Some(path.clone()),
                                    ..ParamFrom::default()
                                }),
                            });
                            name
                        }
                    };
                    format!("{{{{{}}}}}", name)
                } else {
                    links
                        .references
                        .push(json!({ "request": id, "from": from, "path": path }));
                    format!("{{{{@{}.{}}}}}", from, path)
                };
                req.url = replace_value(&req.url, &value, &placeholder);
                req.body = req
                    .body
                    .as_deref()
                    .map(|b| replace_value(b, &value, &placeholder));
            }
        }
    }
    links
}

/// Request ids a request needs answers from, in run order.
pub fn dependencies(flow: &Flow, id: &str) -> Vec<String> {
    // A flow with no output answers with nothing in particular, so a run
    // sends every request and every write goes through the gate.
    if id.is_empty() {
        return flow.requests().map(|r| r.id.clone()).collect();
    }
    let refs_of = |rid: &str| -> Vec<String> {
        flow.request(rid)
            .map(|r| {
                let text = format!("{} {}", r.url, r.body.as_deref().unwrap_or(""));
                text.match_indices("{{@")
                    .filter_map(|(i, _)| text[i + 3..].split('.').next().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut needed = vec![id.to_string()];
    let mut i = 0;
    while i < needed.len() {
        for dep in refs_of(&needed[i].clone()) {
            if !needed.contains(&dep) {
                needed.push(dep);
            }
        }
        i += 1;
    }
    flow.requests()
        .map(|r| r.id.clone())
        .filter(|r| needed.iter().any(|n| n == r))
        .collect()
}

/// Field names an agent can pass to `--where` and `--fields`: the keys of the
/// first item of the response's main list, with one level of dotted paths.
fn item_fields(body: &Value) -> Vec<String> {
    let Some((_, items)) = main_list(body) else {
        return Vec::new();
    };
    let Some(first) = items.first().and_then(|i| i.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (k, v) in first {
        match v.as_object() {
            Some(child) if !child.is_empty() => {
                out.extend(child.keys().map(|c| format!("{}.{}", k, c)))
            }
            _ => out.push(k.clone()),
        }
    }
    out
}

/// The request the flow answers with when the agent does not pick one: the
/// largest response among requests that use a param, else the largest.
fn default_output(steps: &[Step]) -> String {
    let requests: Vec<&Request> = steps.iter().flat_map(|s| s.requests.iter()).collect();
    let uses_param =
        |r: &&Request| r.url.contains("{{") || r.body.as_deref().is_some_and(|b| b.contains("{{"));
    let pool: Vec<&Request> = if requests.iter().any(uses_param) {
        requests.iter().copied().filter(uses_param).collect()
    } else {
        requests
    };
    pool.iter()
        .max_by_key(|r| r.expect.bytes)
        .map(|r| r.id.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Output shaping
// ---------------------------------------------------------------------------

/// `--where field=value` (case-insensitive substring), `--fields a,b.c`, `--limit n`.
#[derive(Default)]
pub struct Shape {
    pub filters: Vec<(String, String)>,
    pub fields: Vec<String>,
    pub limit: Option<usize>,
}

impl Shape {
    pub fn from_cmd(cmd: &Value) -> Shape {
        let filters = cmd
            .get("where")
            .and_then(|v| v.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_lowercase()))
                    .collect()
            })
            .unwrap_or_default();
        let fields = cmd
            .get("fields")
            .and_then(|v| v.as_str())
            .map(|f| {
                f.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let limit = cmd
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        Shape {
            filters,
            fields,
            limit,
        }
    }

    fn is_empty(&self) -> bool {
        self.filters.is_empty() && self.fields.is_empty() && self.limit.is_none()
    }
}

fn lookup<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, key| cur.get(key))
}

fn as_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The list the response is about: the body itself if it is an array, else
/// the heaviest list of objects anywhere in it (GraphQL nests it under `data`).
fn main_list(body: &Value) -> Option<(Option<String>, &Vec<Value>)> {
    if let Some(items) = body.as_array() {
        return Some((None, items));
    }
    let mut best: Option<(String, &Vec<Value>)> = None;
    collect_lists(body, "", &mut best);
    best.map(|(k, v)| (Some(k), v))
}

fn collect_lists<'a>(v: &'a Value, path: &str, best: &mut Option<(String, &'a Vec<Value>)>) {
    let Some(map) = v.as_object() else { return };
    for (k, child) in map {
        let here = if path.is_empty() {
            k.clone()
        } else {
            format!("{}.{}", path, k)
        };
        match child {
            Value::Array(items) if items.first().is_some_and(|i| i.is_object()) => {
                // The heaviest list, not the longest: GraphQL metadata (cache
                // hints, errors) can have more entries than the data.
                let weight =
                    |l: &Vec<Value>| serde_json::to_string(l).map(|t| t.len()).unwrap_or(0);
                if best.as_ref().is_none_or(|(_, b)| weight(items) > weight(b)) {
                    *best = Some((here, items));
                }
            }
            Value::Object(_) => collect_lists(child, &here, best),
            _ => {}
        }
    }
}

/// Whether the list at the recorded path in `body` is gone: absent or no
/// longer an array. A list that is present and empty is not gone.
pub(crate) fn list_gone(path: &str, body: &Value) -> bool {
    !lookup(body, path).is_some_and(|v| v.is_array())
}

/// Field names used in `--where` or `--fields` that no item has.
pub fn unknown_fields(body: &Value, shape: &Shape) -> Vec<String> {
    let Some((_, items)) = main_list(body) else {
        return Vec::new();
    };
    shape
        .filters
        .iter()
        .map(|(f, _)| f)
        .chain(shape.fields.iter())
        .filter(|f| !items.iter().any(|item| lookup(item, f).is_some()))
        .cloned()
        .collect()
}

pub fn apply_shape(body: Value, shape: &Shape) -> Value {
    if shape.is_empty() {
        return body;
    }
    let Some((key, items)) = main_list(&body) else {
        return body;
    };
    let total = items.len();
    let mut kept: Vec<Value> = items
        .iter()
        .filter(|item| {
            shape.filters.iter().all(|(path, needle)| {
                lookup(item, path)
                    .map(|v| as_text(v).to_lowercase().contains(needle.as_str()))
                    .unwrap_or(false)
            })
        })
        .map(|item| {
            if shape.fields.is_empty() {
                return item.clone();
            }
            let mut out = Map::new();
            for f in &shape.fields {
                if let Some(v) = lookup(item, f) {
                    out.insert(f.clone(), v.clone());
                }
            }
            Value::Object(out)
        })
        .collect();
    let matched = kept.len();
    if let Some(limit) = shape.limit {
        kept.truncate(limit);
    }
    json!({ "list": key, "total": total, "matched": matched, "items": kept })
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

pub fn flows_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AGENT_BROWSER_FLOWS_DIR") {
        return PathBuf::from(dir);
    }
    std::env::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".agent-browser")
        .join("flows")
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "invalid flow name '{}': use letters, digits, '-' or '_'",
            name
        ));
    }
    Ok(())
}

pub fn flow_path(origin: &str, name: &str) -> Result<PathBuf, String> {
    validate_origin(origin)?;
    validate_name(name)?;
    Ok(flows_dir().join(origin).join(format!("{}.json", name)))
}

/// Flow files hold recorded URLs and form values, so only the owner can read
/// them, from the moment they are created.
#[cfg(unix)]
fn create_private_dirs(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    for d in [dir, flows_dir().as_path()] {
        if d.starts_with(flows_dir()) && d.exists() {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dirs(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}

pub fn write_flow(flow: &Flow) -> Result<PathBuf, String> {
    let path = flow_path(&flow.origin, &flow.name)?;
    if let Some(parent) = path.parent() {
        create_private_dirs(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    let text = serde_json::to_string_pretty(flow).map_err(|e| e.to_string())?;
    write_private(&path, &text)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    Ok(path)
}

/// A flow file on disk, with the directory it was found in.
struct StoredFlow {
    dir_origin: String,
    flow: Flow,
}

impl StoredFlow {
    /// The directory, the file's origin and its start URL must agree.
    fn is_consistent(&self) -> bool {
        self.flow.origin == self.dir_origin
            && host_of(&self.flow.start_url).as_deref() == Some(self.dir_origin.as_str())
    }
}

fn stored_flows(origin: Option<&str>) -> Vec<StoredFlow> {
    let mut out = Vec::new();
    let Ok(origins) = std::fs::read_dir(flows_dir()) else {
        return out;
    };
    let mut dirs: Vec<_> = origins.flatten().filter(|d| d.path().is_dir()).collect();
    dirs.sort_by_key(|d| d.file_name());
    for dir in dirs {
        let dir_origin = dir.file_name().to_string_lossy().to_string();
        if let Some(o) = origin {
            if base_domain(o) != base_domain(&dir_origin) {
                continue;
            }
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        let mut files: Vec<_> = files.flatten().collect();
        files.sort_by_key(|f| f.file_name());
        for f in files {
            let Ok(text) = std::fs::read_to_string(f.path()) else {
                continue;
            };
            let Ok(flow) = serde_json::from_str::<Flow>(&text) else {
                continue;
            };
            out.push(StoredFlow {
                dir_origin: dir_origin.clone(),
                flow,
            });
        }
    }
    out
}

/// Saved flows as the summaries `flow list`, the hint and WebMCP show.
pub fn list_flows(origin: Option<&str>) -> Vec<Value> {
    stored_flows(origin)
        .into_iter()
        .map(|stored| {
            let flow = &stored.flow;
            json!({
                "name": flow.name,
                "origin": stored.dir_origin,
                "valid": stored.is_consistent(),
                "params": flow.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                "examples": flow.params.iter()
                    .map(|p| (p.name.clone(), json!(p.example.clone().unwrap_or_else(|| "<value>".to_string()))))
                    .collect::<Map<String, Value>>(),
                "description": tool_description(flow),
                "inputSchema": input_schema(flow),
                "paramSources": flow.params.iter()
                    .filter_map(|p| {
                        let from = p.from.as_ref()?;
                        Some((p.name.clone(), json!(format!("{} {}", from.request.as_ref()?, from.list.as_ref()?))))
                    })
                    .collect::<Map<String, Value>>(),
                "itemFields": flow.item_fields,
                "recordedOn": flow.start_url,
                "provenance": flow.provenance,
                "verifiedAt": flow.verified_at,
            })
        })
        .collect()
}

/// A one-line description in the shape of a WebMCP tool's.
fn tool_description(flow: &Flow) -> String {
    if let Some(text) = flow.description.as_deref().filter(|t| !t.trim().is_empty()) {
        return text.chars().take(300).collect();
    }
    let mut out = format!("Replays the task recorded on {}", flow.start_url);
    if !flow.item_fields.is_empty() {
        out.push_str(&format!(
            "; returns items with {}",
            flow.item_fields.join(", ")
        ));
    }
    out
}

/// The flow's params as a JSON Schema, the same shape WebMCP tools declare.
/// A param with a recorded value is optional; a typed one must be passed.
pub fn input_schema(flow: &Flow) -> Value {
    let mut properties = Map::new();
    for p in &flow.params {
        let mut description = match p.example.as_deref() {
            Some(example) => format!("Recorded value: {}", example),
            None => "Typed during the recording; pass a value".to_string(),
        };
        if let Some(list) = p.from.as_ref().and_then(|f| f.list.as_deref()) {
            description.push_str(&format!(". One of the site's {} values", list));
        }
        properties.insert(
            p.name.clone(),
            json!({ "type": "string", "description": description }),
        );
    }
    json!({ "type": "object", "properties": properties, "additionalProperties": false })
}

/// Why a file named like the flow cannot be read, when one exists.
fn unreadable_flow(name: &str, origin: Option<&str>) -> Option<String> {
    let dirs = std::fs::read_dir(flows_dir()).ok()?;
    for dir in dirs.flatten().filter(|d| d.path().is_dir()) {
        let host = dir.file_name().to_string_lossy().to_string();
        if origin.is_some_and(|o| base_domain(o) != base_domain(&host)) {
            continue;
        }
        let path = dir.path().join(format!("{}.json", name));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Err(e) = serde_json::from_str::<Flow>(&text) {
            return Some(format!("{} is not a valid flow: {}", path.display(), e));
        }
    }
    None
}

pub fn load_flow(name: &str, origin: Option<&str>) -> Result<Flow, String> {
    validate_name(name)?;
    let mut matches: Vec<StoredFlow> = stored_flows(origin)
        .into_iter()
        .filter(|s| s.flow.name == name)
        .collect();
    match matches.len() {
        0 => Err(unreadable_flow(name, origin).unwrap_or_else(|| {
            format!(
                "no saved flow named '{}'. Run `flow list` to see saved flows",
                name
            )
        })),
        1 => {
            let stored = matches.remove(0);
            if !stored.is_consistent() {
                return Err(format!(
                    "flow '{}' does not match its origin directory {}; re-save it",
                    name, stored.dir_origin
                ));
            }
            if stored.flow.version != FLOW_VERSION {
                return Err(format!(
                    "flow '{}' has unsupported version {}",
                    name, stored.flow.version
                ));
            }
            // A request that changes state must be listed as a write, or a run
            // would send it with no gate.
            if let Some(r) = stored.flow.requests().find(|r| {
                is_write(&r.method, r.body.as_deref()) && stored.flow.write(&r.id).is_none()
            }) {
                return Err(format!(
                    "flow '{}' sends {} {} without listing it as a write; re-save it",
                    name, r.method, r.url
                ));
            }
            Ok(stored.flow)
        }
        _ => Err(format!(
            "flow '{}' exists for several origins ({}); pass --origin",
            name,
            matches
                .iter()
                .map(|m| m.dir_origin.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct PreparedRequest {
    pub id: String,
    pub step: usize,
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub expect: Expect,
    /// A run needs this request allowed before it sends it.
    pub write: bool,
    /// The templates the text above was bound from; references stay
    /// unbound until the run has the responses.
    pub parts: Option<Parts>,
    /// Headers (lowercase name, value) that the flow records identically on two
    /// or more requests: those do not change per request.
    pub repeated_headers: Vec<(String, String)>,
}

/// Requests to send for a run: the one the flow answers with, and the ones
/// whose responses it takes values from.
pub fn prepare(
    flow: &Flow,
    cmd_params: &[(String, String)],
) -> Result<Vec<PreparedRequest>, String> {
    let needed = dependencies(flow, &flow.output);
    Ok(prepare_all(flow, cmd_params)?
        .into_iter()
        .filter(|r| needed.contains(&r.id))
        .collect())
}

pub fn prepare_all(
    flow: &Flow,
    cmd_params: &[(String, String)],
) -> Result<Vec<PreparedRequest>, String> {
    let mut params: HashMap<String, String> = flow
        .params
        .iter()
        .filter_map(|p| Some((p.name.clone(), p.example.clone()?)))
        .collect();
    let known: Vec<String> = flow.params.iter().map(|p| p.name.clone()).collect();
    for (k, v) in cmd_params {
        if !known.contains(k) {
            return Err(format!(
                "flow '{}' has no param '{}' (params: {})",
                flow.name,
                k,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        }
        params.insert(k.clone(), v.clone());
    }

    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for r in flow.requests() {
        for (k, v) in &r.headers {
            *seen.entry((k.to_ascii_lowercase(), v.clone())).or_default() += 1;
        }
    }
    let repeated: Vec<(String, String)> = seen
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(kv, _)| kv)
        .collect();

    let mut out = Vec::new();
    for (index, step) in flow.steps.iter().enumerate() {
        for r in &step.requests {
            let header_list: Vec<(String, String)> = r
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let headers = header_list
                .iter()
                .filter_map(|(k, v)| {
                    let bound =
                        template::bind_params(&template::parse(v), Slot::Raw, None, &params);
                    Some((k.clone(), template::show(&bound.ok()?)))
                })
                .collect();
            let url = template::bind_params(&template::parse(&r.url), Slot::Url, None, &params)?;
            let body = match &r.body {
                Some(b) => Some(Body::parse(b, &header_list).bind_params(&params)?),
                None => None,
            };
            let url_text = template::show(&url);
            let body_text = body.as_ref().map(|b| b.show());
            let write = flow.write(&r.id).is_some();
            if !write && becomes_write(&r.method, &url_text, body_text.as_deref()) {
                return Err(not_listed_as_write(&r.id));
            }
            out.push(PreparedRequest {
                id: r.id.clone(),
                step: index + 1,
                method: r.method.clone(),
                url: url_text,
                headers,
                body: body_text,
                expect: r.expect.clone(),
                write,
                parts: Some(Parts { url, body }),
                repeated_headers: repeated.clone(),
            });
        }
    }
    Ok(out)
}

impl PreparedRequest {
    /// Fill reference placeholders from the responses received so far.
    pub fn resolve(&self, bodies: &HashMap<String, Value>) -> Result<PreparedRequest, String> {
        let parts = self
            .parts
            .clone()
            .unwrap_or_else(|| Parts::of_text(&self.url, self.body.as_deref(), &self.headers));
        let mut out = self.clone();
        out.url = template::bind_refs(&parts.url, Slot::Url, None, bodies)?;
        if let Some(body) = &parts.body {
            out.body = Some(body.bind_refs(bodies)?);
        }
        if !out.write && becomes_write(&out.method, &out.url, out.body.as_deref()) {
            return Err(not_listed_as_write(&out.id));
        }
        Ok(out)
    }
}

/// A request that is a write, or a GET whose path names a state change.
fn becomes_write(method: &str, url: &str, body: Option<&str>) -> bool {
    is_write(method, body) || (method == "GET" && has_mutating_segment(url))
}

fn not_listed_as_write(id: &str) -> String {
    format!(
        "request {} is a write once its values are filled in, but the flow does not list it as one; it was not sent",
        id
    )
}

const MUTATING_SEGMENTS: &[&str] = &["logout", "signout", "unsubscribe", "delete", "remove"];

/// Whether a path has a whole segment that names a state change. Substrings
/// do not count: `/address` is not `/add`.
fn has_mutating_segment(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| {
        u.path_segments().is_some_and(|mut segments| {
            segments.any(|s| MUTATING_SEGMENTS.contains(&s.to_ascii_lowercase().as_str()))
        })
    })
}

/// The writes a recording caused, kept as requests or not: each one is
/// sent again by a run (or by the command replay) and must be allowed.
/// `suspected` marks a GET whose path has a whole mutating segment.
fn collect_writes(
    input: &SaveInput<'_>,
    policy: &HostPolicy,
    given: &[(String, String)],
    har_ids: &HashMap<String, String>,
) -> Vec<Write> {
    let mut writes: Vec<Write> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, (seq, _)) in input.commands.iter().enumerate() {
        let mut extra = 0;
        for e in input
            .entries
            .iter()
            .filter(|e| input.recorder.request_tags.get(&e.request_id) == Some(seq))
        {
            if carries_secret(e, &input.recorder.secrets)
                || input.recorder.unknown_secret_seqs.contains(seq)
                || is_telemetry(&e.url)
            {
                continue;
            }
            let kept = har_ids
                .iter()
                .find(|(_, har)| **har == e.request_id)
                .map(|(id, _)| id.clone());
            let first_party = policy.allows_url(&e.url);
            let explicit = is_write(&e.method, e.post_data.as_deref());
            let suspected =
                !explicit && e.method == "GET" && first_party && has_mutating_segment(&e.url);
            if !explicit && !suspected {
                continue;
            }
            let key = format!(
                "{} {} {}",
                e.method,
                e.url,
                e.post_data.as_deref().unwrap_or("")
            );
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let id = kept.unwrap_or_else(|| {
                extra += 1;
                format!("s{}w{}", index + 1, extra)
            });
            writes.push(Write {
                id,
                method: e.method.clone(),
                url: template::templatize(&e.url, given, Slot::Url),
                kind: if explicit {
                    WriteKind::Explicit
                } else {
                    WriteKind::Suspected
                },
                step: index + 1,
            });
        }
    }
    writes
}

impl Flow {
    /// The writes a run sends over HTTP or from the page: the ones among the
    /// requests the output needs. Each must be allowed or confirmed first.
    pub fn run_writes(&self) -> Vec<&Write> {
        let needed = dependencies(self, &self.output);
        self.writes
            .iter()
            .filter(|w| needed.contains(&w.id))
            .collect()
    }

    /// Writes the command replay would send beyond the ones a run sends itself.
    pub fn replay_only_writes(&self) -> Vec<&Write> {
        let own = self.run_writes();
        self.writes
            .iter()
            .filter(|w| !own.iter().any(|o| o.id == w.id))
            .collect()
    }
}

/// Which writes `--allow-write` covers: `all`, or a list of write ids.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct AllowWrite {
    pub all: bool,
    pub ids: Vec<String>,
}

impl AllowWrite {
    pub fn parse(value: Option<&Value>) -> Result<AllowWrite, String> {
        let usage = "--allow-write takes `all` or comma-separated write ids";
        let items: Vec<String> = match value {
            None | Some(Value::Null) | Some(Value::Bool(false)) => return Ok(AllowWrite::default()),
            Some(Value::String(text)) => text.split(',').map(|t| t.trim().to_string()).collect(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| i.as_str().map(|t| t.trim().to_string()).ok_or(usage))
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(usage.to_string()),
        };
        let items: Vec<String> = items.into_iter().filter(|i| !i.is_empty()).collect();
        if items.is_empty() {
            return Err(usage.to_string());
        }
        if items.iter().any(|i| i == "all") {
            return Ok(AllowWrite {
                all: true,
                ids: Vec::new(),
            });
        }
        Ok(AllowWrite {
            all: false,
            ids: items,
        })
    }

    pub fn covers(&self, id: &str) -> bool {
        self.all || self.ids.iter().any(|i| i == id)
    }

    /// Every id given must name a write of the flow.
    pub fn check(&self, flow: &Flow) -> Result<(), String> {
        for id in &self.ids {
            if flow.write(id).is_none() {
                let known: Vec<&str> = flow.writes.iter().map(|w| w.id.as_str()).collect();
                return Err(format!(
                    "--allow-write names '{}', which is not a write of this flow. Writes: {}",
                    id,
                    if known.is_empty() {
                        "none".to_string()
                    } else {
                        known.join(", ")
                    }
                ));
            }
        }
        Ok(())
    }
}

pub fn is_write(method: &str, body: Option<&str>) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS") && !is_graphql_query(body)
}

/// A GraphQL POST that only runs queries reads data; mutations still count
/// as writes, and so does an operation with a placeholder in its text, since
/// a value could turn it into one.
fn is_graphql_query(body: Option<&str>) -> bool {
    let Some(parsed) = body.and_then(|b| serde_json::from_str::<Value>(b).ok()) else {
        return false;
    };
    let ops: Vec<&Value> = match &parsed {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    !ops.is_empty()
        && ops.iter().all(|op| {
            op["query"].as_str().is_some_and(|q| {
                let q = q.trim_start();
                (q.starts_with("query") || q.starts_with('{'))
                    && !q.contains("mutation")
                    && !q.contains("{{")
            })
        })
}

/// A flow from a partial fixture: the required fields are filled in.
#[cfg(test)]
pub(crate) fn flow_of(mut v: Value) -> Flow {
    let o = v.as_object_mut().unwrap();
    for (k, d) in [
        ("version", json!(FLOW_VERSION)),
        ("name", json!("f")),
        ("origin", json!("a.test")),
        ("startUrl", json!("https://a.test/")),
        ("steps", json!([])),
    ] {
        o.entry(k).or_insert(d);
    }
    serde_json::from_value(v).unwrap_or_else(|e| panic!("fixture is not a flow: {e}"))
}

#[cfg(test)]
mod tests {
    use super::page::*;
    use super::*;

    fn build_flow_value(input: SaveInput<'_>) -> Result<(Value, Value), String> {
        build_flow(input).map(|(f, r)| (serde_json::to_value(&f).unwrap(), r))
    }

    fn entry(id: &str, url: &str, rtype: &str, mime: &str, body: &str) -> HarEntry {
        HarEntry {
            request_id: id.to_string(),
            wall_time: 0.0,
            method: "GET".to_string(),
            url: url.to_string(),
            request_headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("Cookie".to_string(), "session=secret".to_string()),
                ("X-CSRF-Token".to_string(), "abc".to_string()),
            ],
            post_data: None,
            request_body_size: 0,
            resource_type: rtype.to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/2.0".to_string(),
            response_headers: Vec::new(),
            mime_type: mime.to_string(),
            redirect_url: String::new(),
            response_body_size: body.len() as i64,
            cdp_timing: None,
            loading_finished_timestamp: None,
            response_body: Some(body.to_string()),
            response_body_base64: false,
        }
    }

    fn recorder() -> FlowRecorder {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "id": "1", "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_now("doc");
        r.tag_now("api");
        r.tag_now("ga");
        r.begin_command(
            "click",
            &json!({ "id": "2", "action": "click", "selector": "@e4" }),
            None,
            None,
        );
        r
    }

    #[test]
    fn base_domain_handles_second_level_country_domains() {
        assert_eq!(base_domain("api.example.com.ar"), "example.com.ar");
        assert_eq!(base_domain("www.example.org"), "example.org");
        assert_eq!(base_domain("example.org"), "example.org");
        assert_eq!(base_domain("app.example.io"), "example.io");
    }

    #[test]
    fn save_keeps_first_party_json_and_drops_credentials() {
        let rec = recorder();
        let entries = vec![
            entry(
                "doc",
                "https://www.example.com.ar/",
                "Document",
                "text/html",
                "<html>",
            ),
            entry(
                "api",
                "https://api.example.com.ar/movies?film=Film%20One",
                "Fetch",
                "application/json",
                r#"{"movies":[{"id":1}],"total":1}"#,
            ),
            entry(
                "ga",
                "https://www.google-analytics.com/collect",
                "XHR",
                "application/json",
                "{}",
            ),
        ];
        let params = vec![("film".to_string(), "Film One".to_string())];
        let (flow, report) = build_flow_value(SaveInput {
            name: "showtimes",
            recorder: &rec,
            commands: &rec.commands,
            description: None,
            entries: &entries,
            params: &params,
            output: None,
            created_at: "2026-10-26T00:00:00Z".to_string(),
            login_wall: false,
        })
        .unwrap();
        let req = &flow["steps"][0]["requests"][0];
        assert_eq!(
            req["url"],
            "https://api.example.com.ar/movies?film={{film}}"
        );
        assert!(req["headers"].get("Cookie").is_none());
        assert!(req["headers"].get("X-CSRF-Token").is_none());
        assert_eq!(req["expect"]["strippedAuth"], true);
        assert_eq!(req["expect"]["list"], "movies");
        assert_eq!(flow["steps"][0]["requests"].as_array().unwrap().len(), 1);
        assert_eq!(flow["output"], "s1r1");
        assert_eq!(flow["verifiedAt"], Value::Null);
        assert_eq!(flow["allowedHosts"], json!(["example.com.ar"]));
        assert_eq!(flow["version"], FLOW_VERSION);
        assert!(!serde_json::to_string(&flow).unwrap().contains("secret"));
        assert_eq!(report["gaps"][0]["step"], 2);
        assert_eq!(
            report["gaps"][0]["reason"],
            "no network traffic while this command ran"
        );
    }

    #[test]
    fn save_fails_without_api_traffic() {
        let rec = recorder();
        let entries = vec![entry(
            "doc",
            "https://www.example.com.ar/",
            "Document",
            "text/html",
            "<html>",
        )];
        let err = build_flow_value(SaveInput {
            name: "showtimes",
            recorder: &rec,
            commands: &rec.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap_err();
        assert!(err.contains("no first-party JSON requests"));
    }

    #[test]
    fn signature_detects_removed_and_retyped_keys() {
        let rec = signature(&json!({ "movies": [], "total": 1 }));
        assert!(signature_matches(
            &rec,
            &signature(&json!({ "movies": [], "total": 2, "extra": true }))
        )
        .is_ok());
        assert!(signature_matches(&rec, &signature(&json!({ "total": 2 })))
            .unwrap_err()
            .contains("movies"));
        assert!(signature_matches(&rec, &signature(&json!({ "movies": {}, "total": 2 }))).is_err());
        assert!(signature_matches(&rec, &signature(&json!([1]))).is_err());
    }

    #[test]
    fn shape_filters_by_substring_and_picks_fields() {
        let body = json!({ "data": [
            { "movieName": "FILM ALPHA", "room": "5", "occupation": { "free": 10 } },
            { "movieName": "FILM BETA", "room": "2", "occupation": { "free": 3 } },
        ] });
        let shape = Shape {
            filters: vec![("movieName".to_string(), "alpha".to_string())],
            fields: vec!["room".to_string(), "occupation.free".to_string()],
            limit: None,
        };
        let out = apply_shape(body, &shape);
        assert_eq!(out["total"], 2);
        assert_eq!(out["matched"], 1);
        assert_eq!(
            out["items"][0],
            json!({ "room": "5", "occupation.free": 10 })
        );
    }

    #[test]
    fn signature_catches_nested_changes() {
        let rec =
            signature(&json!({ "data": [{ "movieName": "x", "occupation": { "seats": 1 } }] }));
        let moved = signature(&json!({ "data": [{ "title": "x", "occupation": { "seats": 1 } }] }));
        assert!(signature_matches(&rec, &moved)
            .unwrap_err()
            .contains("data[].movieName"));
    }

    #[test]
    fn prepare_fills_params_and_rejects_unknown_ones() {
        let flow = flow_of(json!({
            "name": "showtimes",
            "params": [{ "name": "film", "example": "Film One" }],
            "output": "s1r1",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://site.example/m?film={{film}}", "headers": {}, "body": null, "expect": {} }] }],
        }));
        let reqs = prepare(&flow, &[("film".to_string(), "Film Two".to_string())]).unwrap();
        assert_eq!(reqs[0].url, "https://site.example/m?film=Film%20Two");
        let reqs = prepare(&flow, &[]).unwrap();
        assert_eq!(reqs[0].url, "https://site.example/m?film=Film%20One");
        assert!(prepare(&flow, &[("city".to_string(), "x".to_string())]).is_err());
    }

    #[test]
    fn recorder_ignores_flow_and_har_commands() {
        let mut r = FlowRecorder::default();
        r.begin_command("click", &json!({}), None, None);
        assert!(r.commands.is_empty());
        r.begin_capture(None);
        r.begin_command("flow_save", &json!({}), None, None);
        r.begin_command("har_start", &json!({}), None, None);
        r.begin_command(
            "click",
            &json!({ "id": "9", "action": "click" }),
            None,
            None,
        );
        assert_eq!(r.commands.len(), 1);
        assert!(r.commands[0].1.get("id").is_none());
    }

    fn expect_of(v: Value) -> Expect {
        serde_json::from_value(v).unwrap()
    }

    fn prepared(id: &str) -> PreparedRequest {
        PreparedRequest {
            id: id.to_string(),
            step: 1,
            method: "GET".to_string(),
            url: "https://api.site.example/m?film=Film".to_string(),
            headers: vec![
                ("User-Agent".to_string(), "x".to_string()),
                ("sec-ch-ua".to_string(), "x".to_string()),
                ("country".to_string(), "AR".to_string()),
            ],
            body: None,
            expect: expect_of(json!({ "signature": signature(&json!({ "data": [{ "id": 1 }] })) })),
            write: false,
            parts: None,
            repeated_headers: Vec::new(),
        }
    }

    fn attempt(id: &str, tier: Tier, outcome: Result<(u16, String), Failure>) -> TierResult {
        tier_result(&prepared(id), tier, Instant::now(), outcome)
    }

    #[test]
    fn a_tier_that_got_a_page_instead_of_data_falls_back_and_the_flow_stays_verified() {
        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r1", "verifiedAt": null,
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        let attempts = vec![
            attempt(
                "s1r1",
                Tier::Http,
                Ok((200, "<html>sign in</html>".to_string())),
            ),
            attempt(
                "s1r1",
                Tier::Page,
                Ok((200, r#"{"data":[{"id":1}]}"#.to_string())),
            ),
        ];
        let out = finish_run(&mut flow, &attempts, "now", 1, &Shape::default());
        assert_eq!(out["verified"], true);
        assert_eq!(out["steps"][0]["tier"], "page");
        assert_eq!(out["steps"][0]["fellBack"][0]["tier"], "http");
        assert_eq!(flow.verified_at.as_deref(), Some("now"));
        assert_eq!(flow.steps[0].requests[0].tier, Some(Tier::Page));
        assert_eq!(tiers_for(&flow, "s1r1"), vec![Tier::Page, Tier::Replay]);
    }

    #[test]
    fn a_changed_shape_marks_the_flow_unverified_even_if_a_lower_tier_answers() {
        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r1", "verifiedAt": "before",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        let attempts = vec![
            attempt("s1r1", Tier::Http, Ok((200, r#"{"items":[]}"#.to_string()))),
            attempt(
                "s1r1",
                Tier::Page,
                Ok((200, r#"{"data":[{"id":1}]}"#.to_string())),
            ),
        ];
        let out = finish_run(&mut flow, &attempts, "now", 1, &Shape::default());
        assert_eq!(out["ok"], true);
        assert_eq!(out["verified"], false);
        assert_eq!(out["drift"]["tier"], "http");
        assert_eq!(flow.verified_at, None);
    }

    fn catalog_request() -> PreparedRequest {
        let mut req = prepared("s1r1");
        req.expect = expect_of(json!({
            "signature": signature(&json!({ "items": [{ "name": "x", "price": 1 }], "total": 1 })),
        }));
        req
    }

    #[test]
    fn a_list_that_moved_is_drift_not_an_empty_result() {
        let moved =
            r#"{"data":{"products":[{"title":"x","amount":{"value":1,"currency":"USD"}}]}}"#;
        let result = tier_result(
            &catalog_request(),
            Tier::Http,
            Instant::now(),
            Ok((200, moved.to_string())),
        );
        assert!(!result.ok, "{:?}", result.reason);
        assert!(result.shape_changed);
        let reason = result.reason.clone().unwrap();
        assert!(
            reason.contains("'items'") && reason.contains("data.products"),
            "{reason}"
        );
        let outcome = RunOutcome::failed(&[result], &flow_of(json!({})));
        let message = outcome.message("catalog");
        assert!(message.starts_with("drifted: "), "{message}");
        assert!(message.contains("save the flow again"), "{message}");
        assert_eq!(outcome.data("catalog").unwrap()["status"], "drifted");
    }

    fn drift_of(rec: Value, body: &str) -> TierResult {
        let mut req = prepared("s1r1");
        req.expect = expect_of(json!({ "signature": signature(&rec) }));
        tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Ok((200, body.to_string())),
        )
    }

    #[test]
    fn a_missing_recorded_list_is_drift_and_an_empty_one_is_not() {
        let flat = json!({ "items": [{ "name": "x", "price": 1 }], "total": 1 });
        let nested =
            json!({ "data": { "search": { "items": [{ "id": 1, "name": "a" }], "total": 1 } } });
        // (recorded, body, drifted, what the message must name)
        let cases = [
            (
                &nested,
                r#"{"data":{"search":{"products":[{"id":1,"name":"a"}]}}}"#,
                true,
                "data.search.items",
            ),
            (
                &nested,
                r#"{"data":{"search":{"products":[{"id":1,"name":"a"}]}}}"#,
                true,
                "a list now appears at 'data.search.products'",
            ),
            (
                &flat,
                r#"{"data":{"products":[{"title":"x"}]}}"#,
                true,
                "a list now appears at 'data.products'",
            ),
            (
                &flat,
                r#"{"facets":{"brand":[]}}"#,
                true,
                "key 'items' is missing",
            ),
            (
                &flat,
                r#"{"meta":{"links":[{"href":"/p2"}]}}"#,
                true,
                "key 'items' is missing",
            ),
            (&flat, "{}", true, "key 'items' is missing"),
            (
                &flat,
                r#"{"items":[],"meta":{"links":[{"href":"/p2"}]}}"#,
                false,
                "",
            ),
            (&flat, r#"{"items":[]}"#, false, ""),
            (&flat, r#"{"items":[],"total":0}"#, false, ""),
            (
                &nested,
                r#"{"data":{"search":{"items":[]}},"extensions":{"warnings":[]}}"#,
                false,
                "",
            ),
        ];
        for (rec, body, drifted, named) in cases {
            let r = drift_of(rec.clone(), body);
            assert_eq!(r.shape_changed, drifted, "{body}: {:?}", r.reason);
            assert_eq!(r.ok, !drifted, "{body}");
            if drifted {
                assert!(
                    r.reason.as_deref().unwrap().contains(named),
                    "{body}: {:?}",
                    r.reason
                );
            }
        }
    }

    #[test]
    fn a_main_list_deeper_than_the_signature_is_still_checked() {
        let rec = json!({ "data": { "search": { "page": { "items": [{ "id": 1, "name": "a" }], "n": 1 } } } });
        let mut req = prepared("s1r1");
        req.expect = expect_of(json!({
            "signature": signature(&rec),
            "list": "data.search.page.items",
        }));
        let judge = |body: &str| check_response(&req, 200, body);
        // Dropped, retyped and renamed.
        for body in [
            r#"{"data":{"search":{"page":{"n":0}}}}"#,
            r#"{"data":{"search":{"page":{"items":{"a":1},"n":1}}}}"#,
            r#"{"data":{"search":{"page":{"items":"none"}}}}"#,
            r#"{"data":{"search":{"page":{"products":[{"id":1,"name":"a"}],"n":1}}}}"#,
            r#"{"data":{"search":{"page":null}}}"#,
        ] {
            let f = judge(body).unwrap_err();
            assert!(f.shape_changed, "{body}");
            assert!(f.reason.contains("data.search.page.items"), "{}", f.reason);
        }
        // Present, empty or not, is fine.
        for body in [
            r#"{"data":{"search":{"page":{"items":[],"n":0}}}}"#,
            r#"{"data":{"search":{"page":{"items":[{"id":2,"name":"b"}],"n":1}}}}"#,
        ] {
            assert!(judge(body).is_ok(), "{body}");
        }
        let f = judge(r#"{"data":{"search":{"page":{"products":[{"id":1}]}}}}"#).unwrap_err();
        assert!(f.reason.contains("a list now appears at"), "{}", f.reason);
    }

    #[test]
    fn a_main_list_under_a_dotted_key_is_not_saved_as_a_path() {
        let rec = recorder();
        let body = r#"{"results.v2":[{"id":1}],"total":1}"#;
        let entries = vec![entry(
            "api",
            "https://api.example.com.ar/movies",
            "Fetch",
            "application/json",
            body,
        )];
        let (flow, _) = build_flow_value(SaveInput {
            name: "dotted",
            recorder: &rec,
            commands: &rec.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let saved: Request =
            serde_json::from_value(flow["steps"][0]["requests"][0].clone()).unwrap();
        let mut req = prepared("s1r1");
        req.expect = saved.expect;
        assert!(check_response(&req, 200, body).is_ok());
    }

    #[test]
    fn an_empty_list_with_the_same_shape_is_not_drift() {
        for body in [r#"{"items":[],"total":0}"#, r#"{"items":[],"page":1}"#] {
            let result = tier_result(
                &catalog_request(),
                Tier::Http,
                Instant::now(),
                Ok((200, body.to_string())),
            );
            assert!(
                result.ok && !result.shape_changed,
                "{body}: {:?}",
                result.reason
            );
        }
    }

    #[test]
    fn a_rejected_input_keeps_the_flow_verified() {
        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r1", "verifiedAt": "before",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        let attempts = vec![attempt("s1r1", Tier::Http, Ok((400, "{}".to_string())))];
        let out = finish_run(&mut flow, &attempts, "now", 1, &Shape::default());
        assert_eq!(out["ok"], false);
        assert_eq!(flow.verified_at.as_deref(), Some("before"));
    }

    #[test]
    fn page_fetch_leaves_browser_headers_to_the_browser() {
        let script = page_fetch_script(&prepared("s1r1"));
        assert!(script.contains("country"));
        assert!(!script.contains("User-Agent"));
        assert!(!script.contains("sec-ch-ua"));
    }

    #[test]
    fn replay_swaps_params_into_commands_and_refuses_missing_ones() {
        let flow = flow_of(json!({
            "params": [{ "name": "q", "example": "film one" }],
            "steps": [
                { "command": { "action": "navigate", "url": "https://site.example/search?q=film%20one" } },
                { "command": { "action": "fill", "selector": "#q", "value": "film one" } },
            ],
        }));
        let cmds = replay_commands(&flow, &[("q".to_string(), "other".to_string())]).unwrap();
        assert_eq!(cmds[0]["url"], "https://site.example/search?q=other");
        assert_eq!(cmds[1]["value"], "other");
        let hidden = flow_of(json!({
            "params": [{ "name": "theater", "example": "401" }],
            "steps": [{ "command": { "action": "navigate", "url": "https://site.example/downtown" } }],
        }));
        assert!(
            replay_commands(&hidden, &[("theater".to_string(), "402".to_string())])
                .unwrap_err()
                .contains("theater")
        );
        assert!(replay_commands(&hidden, &[]).is_ok());
    }

    #[test]
    fn save_turns_list_values_into_params_and_single_values_into_references() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        for id in ["theaters", "token", "seats"] {
            r.tag_now(id);
        }
        let entries = vec![
            entry(
                "theaters",
                "https://api.example.com.ar/theaters",
                "Fetch",
                "application/json",
                r#"{"data":[{"theaterId":"401"},{"theaterId":"402"}]}"#,
            ),
            entry(
                "token",
                "https://api.example.com.ar/session",
                "Fetch",
                "application/json",
                r#"{"session":{"key":"k-81f2"}}"#,
            ),
            entry(
                "seats",
                "https://api.example.com.ar/seats?theater=401&key=k-81f2",
                "Fetch",
                "application/json",
                r#"{"seats":[{"row":"A"}]}"#,
            ),
        ];
        let (flow, report) = build_flow_value(SaveInput {
            name: "seats",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: Some("s1r3"),
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let seats = &flow["steps"][0]["requests"][2];
        assert_eq!(
            seats["url"],
            "https://api.example.com.ar/seats?theater={{theaterId}}&key={{@s1r2.session.key}}"
        );
        assert_eq!(
            report["detectedParams"][0]["from"]["list"],
            "data[].theaterId"
        );
        assert_eq!(report["references"][0]["path"], "session.key");
        assert_eq!(
            dependencies(&flow_of(flow.clone()), "s1r3"),
            vec!["s1r2", "s1r3"]
        );

        let reqs = prepare(
            &flow_of(flow.clone()),
            &[("theaterId".to_string(), "402".to_string())],
        )
        .unwrap();
        assert_eq!(reqs.len(), 2);
        let mut bodies = HashMap::new();
        bodies.insert("s1r2".to_string(), json!({ "session": { "key": "k 2" } }));
        let filled = reqs[1].resolve(&bodies).unwrap();
        assert_eq!(
            filled.url,
            "https://api.example.com.ar/seats?theater=402&key=k%202"
        );
        assert!(reqs[1].resolve(&HashMap::new()).is_err());
    }

    fn flow_of_entries(
        r: &FlowRecorder,
        entries: &[HarEntry],
        output: Option<&str>,
    ) -> Result<(Value, Value), String> {
        build_flow_value(SaveInput {
            name: "t",
            recorder: r,
            commands: &r.commands,
            description: None,
            entries,
            params: &[],
            output,
            created_at: String::new(),
            login_wall: false,
        })
    }

    #[test]
    fn a_response_value_equal_to_a_path_segment_other_requests_share_is_not_a_param() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        r.tag_now("projects");
        r.tag_now("env");
        let entries = vec![
            entry(
                "projects",
                "https://app.example/api/v9/projects",
                "Fetch",
                "application/json",
                r#"{"projects":[{"id":"prj_x","meta":{"gitRootDirectory":"api"}}]}"#,
            ),
            entry(
                "env",
                "https://app.example/api/v10/projects/prj_x/env",
                "Fetch",
                "application/json",
                r#"{"envs":[]}"#,
            ),
        ];
        let (flow, report) = flow_of_entries(&r, &entries, None).unwrap();
        let url = flow["steps"][0]["requests"][1]["url"].as_str().unwrap();
        assert_eq!(url, "https://app.example/api/v10/projects/{{id}}/env");
        assert_eq!(report["params"], json!(["id"]));
    }

    #[test]
    fn output_accepts_the_capture_id_of_a_renumbered_request_and_errors_list_valid_ids() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/a" }),
            None,
            None,
        );
        r.tag_now("one");
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/b" }),
            None,
            None,
        );
        r.tag_now("two");
        let entries = vec![
            entry(
                "one",
                "https://app.example/api/one",
                "Fetch",
                "application/json",
                r#"{"a":1}"#,
            ),
            entry(
                "two",
                "https://app.example/api/two",
                "Fetch",
                "application/json",
                r#"{"b":2}"#,
            ),
        ];
        // Save only the second step: its saved id is s1r1, its capture id s2r1.
        let second = &r.commands[1..];
        let save = |output: Option<&str>| {
            build_flow_value(SaveInput {
                name: "t",
                recorder: &r,
                commands: second,
                description: None,
                entries: &entries,
                params: &[],
                output,
                created_at: String::new(),
                login_wall: false,
            })
        };
        let (flow, _) = save(Some("s2r1")).unwrap();
        assert_eq!(flow["output"], "s1r1");
        assert_eq!(save(Some("s1r1")).unwrap().0["output"], "s1r1");
        let err = save(Some("s9r9")).unwrap_err();
        assert!(err.contains("s1r1") && err.contains("s2r1"), "{err}");
    }

    #[test]
    fn identifying_and_tracing_headers_are_not_saved_in_any_case() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        r.tag_now("api");
        let mut api = entry(
            "api",
            "https://app.example/api/x",
            "Fetch",
            "application/json",
            r#"{"a":1}"#,
        );
        let dropped = [
            "X-Client-IP",
            "x-forwarded-for",
            "X-Real-IP",
            "Forwarded",
            "traceparent",
            "TraceState",
            "X-Request-Id",
            "Request-Id",
            "X-Amzn-Trace-Id",
            "Sentry-Trace",
            "Baggage",
            "CF-Connecting-IP",
            "True-Client-IP",
        ];
        for name in dropped {
            api.request_headers
                .push((name.to_string(), "2001:db8::1".to_string()));
        }
        api.request_headers
            .push(("X-App-Version".to_string(), "7".to_string()));
        let (flow, report) = flow_of_entries(&r, &[api], None).unwrap();
        let headers = flow["steps"][0]["requests"][0]["headers"]
            .as_object()
            .unwrap();
        let kept: Vec<&String> = headers.keys().collect();
        assert_eq!(kept, ["Accept", "X-App-Version"]);
        for name in dropped {
            assert!(report["droppedHeaders"]
                .as_array()
                .unwrap()
                .contains(&json!(name.to_ascii_lowercase())));
        }
    }

    #[test]
    fn a_page_query_value_only_the_referer_carries_is_not_a_param() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/env?create=true" }),
            None,
            None,
        );
        r.tag_now("api");
        // The page document is outside the saved requests.
        let doc = entry(
            "doc",
            "https://app.example/env?create=true",
            "Document",
            "text/html",
            r#"<script id="d" type="application/json">{"a":1}</script>"#,
        );
        let mut api = entry(
            "api",
            "https://app.example/api/env",
            "Fetch",
            "application/json",
            r#"{"envs":[]}"#,
        );
        api.request_headers.push((
            "Referer".to_string(),
            "https://app.example/env?create=true".to_string(),
        ));
        let (flow, report) = flow_of_entries(&r, &[doc, api], None).unwrap();
        assert_eq!(report["params"], json!([]));
        assert_eq!(
            flow["steps"][0]["requests"][0]["headers"]["Referer"],
            "https://app.example/env?create=true"
        );
    }

    #[test]
    fn an_id_two_later_paths_share_is_still_a_param() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        for id in ["projects", "env", "domains"] {
            r.tag_now(id);
        }
        let entries = vec![
            entry(
                "projects",
                "https://app.example/api/v9/projects",
                "Fetch",
                "application/json",
                r#"{"projects":[{"id":"prj_Ab12cd"},{"id":"prj_Zz99yy"}]}"#,
            ),
            entry(
                "env",
                "https://app.example/api/v10/projects/prj_Ab12cd/env",
                "Fetch",
                "application/json",
                r#"{"envs":[1]}"#,
            ),
            entry(
                "domains",
                "https://app.example/api/v9/projects/prj_Ab12cd/domains",
                "Fetch",
                "application/json",
                r#"{"domains":[1,2]}"#,
            ),
        ];
        let (flow, _) = flow_of_entries(&r, &entries, None).unwrap();
        let reqs = &flow["steps"][0]["requests"];
        assert_eq!(
            reqs[1]["url"],
            "https://app.example/api/v10/projects/{{id}}/env"
        );
        assert_eq!(
            reqs[2]["url"],
            "https://app.example/api/v9/projects/{{id}}/domains"
        );
    }

    #[test]
    fn a_detail_response_that_echoes_the_id_does_not_make_it_structure() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        for id in ["list", "detail", "env"] {
            r.tag_now(id);
        }
        let entries = vec![
            entry(
                "list",
                "https://app.example/api/projects",
                "Fetch",
                "application/json",
                r#"{"projects":[{"id":"prj_Ab12cd"},{"id":"prj_Zz99yy"}]}"#,
            ),
            entry(
                "detail",
                "https://app.example/api/projects/prj_Ab12cd",
                "Fetch",
                "application/json",
                r#"{"id":"prj_Ab12cd","name":"a"}"#,
            ),
            entry(
                "env",
                "https://app.example/api/projects/prj_Ab12cd/env",
                "Fetch",
                "application/json",
                r#"{"envs":[1]}"#,
            ),
        ];
        let (flow, _) = flow_of_entries(&r, &entries, None).unwrap();
        let reqs = &flow["steps"][0]["requests"];
        assert_eq!(reqs[0]["url"], "https://app.example/api/projects");
        assert_eq!(reqs[1]["url"], "https://app.example/api/projects/{{id}}");
        assert_eq!(
            reqs[2]["url"],
            "https://app.example/api/projects/{{id}}/env"
        );
        let parsed: Flow = serde_json::from_value(flow).unwrap();
        let run = prepare_all(&parsed, &[("id".to_string(), "prj_Zz99yy".to_string())]).unwrap();
        assert_eq!(run[1].url, "https://app.example/api/projects/prj_Zz99yy");
        assert_eq!(
            run[2].url,
            "https://app.example/api/projects/prj_Zz99yy/env"
        );
    }

    fn bootstrap_then_projects(first: &str) -> Value {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        for id in ["boot", "list", "detail", "env"] {
            r.tag_now(id);
        }
        let entries = vec![
            entry(
                "boot",
                "https://app.example/api/me",
                "Fetch",
                "application/json",
                first,
            ),
            entry(
                "list",
                "https://app.example/api/projects",
                "Fetch",
                "application/json",
                r#"{"projects":[{"id":"prj_Ab12cd"},{"id":"prj_Zz99yy"}]}"#,
            ),
            entry(
                "detail",
                "https://app.example/api/projects/prj_Ab12cd",
                "Fetch",
                "application/json",
                r#"{"id":"prj_Ab12cd","name":"a"}"#,
            ),
            entry(
                "env",
                "https://app.example/api/projects/prj_Ab12cd/env",
                "Fetch",
                "application/json",
                r#"{"envs":[1]}"#,
            ),
        ];
        flow_of_entries(&r, &entries, None).unwrap().0
    }

    #[test]
    fn a_coincidental_bootstrap_value_does_not_replace_the_list_as_source() {
        for first in [
            r#"{"user":{"id":"usr_9xA2"},"lastProjectId":"prj_Ab12cd"}"#,
            r#"{"flags":{"exp_a":"prj_Ab12cd"}}"#,
            r#"{"recent":[{"id":"prj_Ab12cd"}]}"#,
        ] {
            let flow = bootstrap_then_projects(first);
            let reqs = &flow["steps"][0]["requests"];
            assert_eq!(
                reqs[2]["url"], "https://app.example/api/projects/{{id}}",
                "{first}"
            );
            assert_eq!(
                reqs[3]["url"], "https://app.example/api/projects/{{id}}/env",
                "{first}"
            );
            let params = flow["params"].as_array().unwrap();
            assert_eq!(params.len(), 1, "{first}: {params:?}");
            assert_eq!(params[0]["from"]["request"], "s1r2", "{first}");
            assert_eq!(params[0]["from"]["list"], "projects[].id", "{first}");
        }
    }

    #[test]
    fn a_single_value_two_later_paths_share_is_a_reference() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/" }),
            None,
            None,
        );
        for id in ["user", "members", "billing"] {
            r.tag_now(id);
        }
        let entries = vec![
            entry(
                "user",
                "https://app.example/api/user",
                "Fetch",
                "application/json",
                r#"{"user":{"teamId":"team_Q8x7"}}"#,
            ),
            entry(
                "members",
                "https://app.example/teams/team_Q8x7/members",
                "Fetch",
                "application/json",
                r#"{"m":[1]}"#,
            ),
            entry(
                "billing",
                "https://app.example/teams/team_Q8x7/billing",
                "Fetch",
                "application/json",
                r#"{"b":[1,2]}"#,
            ),
        ];
        let (flow, _) = flow_of_entries(&r, &entries, None).unwrap();
        let reqs = &flow["steps"][0]["requests"];
        for i in [1, 2] {
            assert!(reqs[i]["url"]
                .as_str()
                .unwrap()
                .contains("{{@s1r1.user.teamId}}"));
        }
        assert_eq!(
            dependencies(&flow_of(flow.clone()), "s1r2"),
            vec!["s1r1", "s1r2"]
        );
    }

    #[test]
    fn an_output_id_that_means_two_requests_is_refused_with_both() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        for (page, id) in [("a", "one"), ("b", "two"), ("c", "three")] {
            r.begin_command(
                "navigate",
                &json!({ "action": "navigate", "url": format!("https://app.example/{page}") }),
                None,
                None,
            );
            r.tag_now(id);
        }
        let entries: Vec<HarEntry> = ["one", "two", "three"]
            .iter()
            .map(|n| {
                entry(
                    n,
                    &format!("https://app.example/api/{n}"),
                    "Fetch",
                    "application/json",
                    r#"{"a":1}"#,
                )
            })
            .collect();
        let range = &r.commands[1..];
        let save = |output: &str| {
            build_flow_value(SaveInput {
                name: "t",
                recorder: &r,
                commands: range,
                description: None,
                entries: &entries,
                params: &[],
                output: Some(output),
                created_at: String::new(),
                login_wall: false,
            })
        };
        let err = save("s2r1").unwrap_err();
        assert!(
            err.contains("s2r1 is ambiguous: saved s2r1 (https://app.example/api/three)")
                && err.contains("saved as s1r1 (https://app.example/api/two)"),
            "{err}"
        );
        assert_eq!(save("s1r1").unwrap().0["output"], "s1r1");
        assert_eq!(save("s3r1").unwrap().0["output"], "s2r1");
    }

    #[test]
    fn a_referer_keeps_the_placeholders_of_params_that_stay() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://app.example/search?q=tea&tab=recent" }),
            None,
            None,
        );
        r.tag_now("api");
        let doc = entry(
            "doc",
            "https://app.example/search?q=tea&tab=recent",
            "Document",
            "text/html",
            r#"<script id="d" type="application/json">{"a":1}</script>"#,
        );
        let mut api = entry(
            "api",
            "https://app.example/api/search?q=tea",
            "Fetch",
            "application/json",
            r#"{"r":[]}"#,
        );
        api.request_headers.push((
            "Referer".to_string(),
            "https://app.example/search?q=tea&tab=recent".to_string(),
        ));
        let (flow, report) = flow_of_entries(&r, &[doc, api], None).unwrap();
        assert_eq!(report["params"], json!(["q"]));
        assert_eq!(
            flow["steps"][0]["requests"][0]["headers"]["Referer"],
            "https://app.example/search?q={{q}}&tab=recent"
        );
    }

    #[test]
    fn a_login_wall_names_the_last_gap() {
        let rec = recorder();
        let entries = vec![entry(
            "api",
            "https://api.example.com.ar/movies",
            "Fetch",
            "application/json",
            r#"{"movies":[]}"#,
        )];
        let (_, report) = build_flow_value(SaveInput {
            name: "showtimes",
            recorder: &rec,
            commands: &rec.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: true,
        })
        .unwrap();
        assert!(report["gaps"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("login"));
    }

    #[test]
    fn input_schema_describes_params_like_a_webmcp_tool() {
        let flow = flow_of(json!({
            "params": [
                { "name": "q", "example": "kettle" },
                { "name": "storeId", "example": "401", "from": { "request": "s1r1", "list": "stores[].storeId" } },
            ],
        }));
        let schema = input_schema(&flow);
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["q"]["type"], "string");
        assert!(schema["properties"]["storeId"]["description"]
            .as_str()
            .unwrap()
            .contains("stores[].storeId"));
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn replay_match_tells_graphql_operations_apart() {
        let mut a = entry(
            "a",
            "https://api.site.example/graphql",
            "Fetch",
            "application/json",
            "{}",
        );
        a.method = "POST".to_string();
        a.post_data = Some(r#"{"operationName":"appConfig","variables":{}}"#.to_string());
        let mut b = a.clone();
        b.request_id = "b".to_string();
        b.post_data = Some(r#"{"operationName":"positions","variables":{"t":2}}"#.to_string());
        let mut req = prepared("s1r1");
        req.method = "POST".to_string();
        req.url = "https://api.site.example/graphql".to_string();
        req.body = Some(r#"{"operationName":"positions","variables":{"t":1}}"#.to_string());
        let entries = vec![b, a];
        assert_eq!(replay_lookup(&entries, &req).ok().unwrap().request_id, "b");
        req.body = Some(r#"{"operationName":"other"}"#.to_string());
        assert!(replay_lookup(&entries, &req).ok().is_none());
    }

    #[test]
    fn replace_token_leaves_longer_words_alone() {
        assert_eq!(
            replace_token("theater=401&t=14011", "401", "{{t}}"),
            "theater={{t}}&t=14011"
        );
        assert_eq!(
            replace_token("publicSettingsForApp&p=public", "public", "{{p}}"),
            "publicSettingsForApp&p={{p}}"
        );
        assert!(looks_generated("R_kgDOUQ"));
        assert!(!looks_generated("es-AR"));
        assert!(!looks_generated("public"));
    }

    #[test]
    fn a_start_url_query_value_the_api_sent_becomes_a_param() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command("navigate", &json!({ "action": "navigate", "url": "https://www.shop.example/l/?keyword=kettle&page=1" }), None, None);
        r.tag_now("api");
        let mut api = entry(
            "api",
            "https://www.shop.example/api/v2",
            "Fetch",
            "application/json",
            r#"{"items":[]}"#,
        );
        api.method = "POST".to_string();
        api.post_data =
            Some(r#"{"query":"query s { x }","variables":{"keyword":"kettle"}}"#.to_string());
        let (flow, report) = build_flow_value(SaveInput {
            name: "search",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[api],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        assert_eq!(report["params"], json!(["keyword"]));
        assert!(flow["steps"][0]["requests"][0]["body"]
            .as_str()
            .unwrap()
            .contains("{{keyword}}"));
    }

    #[test]
    fn shape_finds_a_list_nested_under_graphql_data() {
        let body = json!({ "data": { "items": { "results": [
            { "item": { "title": "A" }, "price": 1 },
            { "item": { "title": "B" }, "price": 2 },
        ] } }, "extensions": { "hints": [{ "a": 1 }, { "a": 2 }, { "a": 3 }] } });
        let shape = Shape {
            filters: vec![],
            fields: vec!["item.title".to_string()],
            limit: Some(1),
        };
        assert!(unknown_fields(&body, &shape).is_empty());
        let out = apply_shape(body, &shape);
        assert_eq!(out["list"], "data.items.results");
        assert_eq!(out["items"], json!([{ "item.title": "A" }]));
    }

    #[test]
    fn embedded_json_finds_script_blocks_and_assignments() {
        let html = r#"<html><script>console.log(1)</script>
<script id="__NEXT_DATA__" type="application/json">{"props":{"items":[{"id":1}]}}</script>
<script type="application/ld+json">[{"@type":"Product"}]</script>
<script>window.__context__ = {"packages":[{"name":"a"}]};</script></html>"#;
        let blocks = embedded_json(html);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].0, json!({ "scriptId": "__NEXT_DATA__" }));
        assert_eq!(blocks[2].1["packages"][0]["name"], "a");
        let back = extract_embedded(html, &json!({ "scriptIndex": 2 })).unwrap();
        assert!(back.contains("packages"));
        assert!(
            extract_embedded("<html></html>", &json!({ "scriptId": "__NEXT_DATA__" })).is_none()
        );
    }

    #[test]
    fn missing_counts_with_a_populated_list_are_not_empty() {
        let req = PreparedRequest {
            expect: expect_of(json!({
                "list": "items",
                "signature": signature(&json!({ "items": [{ "id": 1 }], "total": 1 })),
            })),
            ..prepared("s1r1")
        };
        let (body, empty) = check_response(&req, 200, r#"{"items":[{"id":1},{"id":2}]}"#).unwrap();
        assert!(!empty);
        assert_eq!(body["items"].as_array().unwrap().len(), 2);
        let (_, empty) = check_response(&req, 200, r#"{"items":[]}"#).unwrap();
        assert!(empty);
        let gone = check_response(&req, 200, r#"{"other":[{"id":1}]}"#).unwrap_err();
        assert!(gone.shape_changed);
    }

    #[test]
    fn a_response_without_its_list_is_drift_and_without_its_counts_is_empty() {
        let req = PreparedRequest {
            expect: expect_of(
                json!({ "signature": signature(&json!({ "payload": { "results": [{ "id": 1 }], "total": 1 } })) }),
            ),
            ..prepared("s1r1")
        };
        let missing = check_response(&req, 200, r#"{"payload":{}}"#).unwrap_err();
        assert!(missing.shape_changed);
        let (_, empty) = check_response(&req, 200, r#"{"payload":{"results":[]}}"#).unwrap();
        assert!(empty);
        let moved = check_response(&req, 200, r#"{"payload":{"items":[]}}"#).unwrap_err();
        assert!(moved.shape_changed);

        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r1", "verifiedAt": "before",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        let attempts = vec![tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Ok((200, r#"{"payload":{"results":[]}}"#.to_string())),
        )];
        let out = finish_run(&mut flow, &attempts, "now", 1, &Shape::default());
        assert_eq!(out["ok"], true);
        assert_eq!(out["empty"], true);
        assert_eq!(out["verified"], false);
        assert_eq!(flow.verified_at.as_deref(), Some("before"));
    }

    #[test]
    fn secrets_are_not_recorded_and_block_the_replay() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "#password", "value": "hunter2" }),
            None,
            None,
        );
        r.begin_command(
            "auth_login",
            &json!({ "action": "auth_login", "name": "x" }),
            None,
            None,
        );
        assert_eq!(r.commands.len(), 1);
        assert_eq!(r.commands[0].1["value"], REDACTED);
        let flow = flow_of(json!({
            "params": [],
            "startUrl": "https://site.example/",
            "steps": [{ "command": r.commands[0].1.clone() }],
        }));
        assert!(replay_commands(&flow, &[]).unwrap_err().contains("secret"));
    }

    #[test]
    fn save_lists_every_write_the_recording_caused() {
        let rec = recorder();
        let mut buy = entry(
            "buy",
            "https://www.example.com.ar/cart?x=1",
            "XHR",
            "text/plain",
            "ok",
        );
        buy.method = "POST".to_string();
        let mut r = rec;
        r.tag_now("buy");
        let entries = vec![
            entry(
                "api",
                "https://api.example.com.ar/movies",
                "Fetch",
                "application/json",
                r#"{"movies":[]}"#,
            ),
            buy,
        ];
        let (flow, _) = build_flow_value(SaveInput {
            name: "f",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let writes = flow["writes"].as_array().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0]["method"], "POST");
        assert_eq!(writes[0]["kind"], "explicit");
        assert_eq!(writes[0]["url"], "https://www.example.com.ar/cart?x=1");
        assert_eq!(writes[0]["id"], "s2w1");
        assert!(
            flow_of(flow.clone()).replay_only_writes().len() == 1
                && write_ids(&flow_of(flow.clone())).is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn flow_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("abflow-perm-{}", std::process::id()));
        let path = dir.join("site.example").join("f.json");
        create_private_dirs(path.parent().unwrap()).unwrap();
        write_private(&path, "{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn typed_secrets_are_redacted_and_their_requests_dropped() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        r.begin_command(
            "type",
            &json!({ "action": "type", "selector": "#password", "text": "hunter2" }),
            None,
            None,
        );
        assert_eq!(r.commands[1].1["text"], REDACTED);
        r.tag_now("login");
        r.tag_now("me");
        let mut login = entry(
            "login",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        login.method = "POST".to_string();
        login.post_data = Some(r#"{"user":"a","password":"hunter2"}"#.to_string());
        let me = entry(
            "me",
            "https://api.example.com.ar/me",
            "Fetch",
            "application/json",
            r#"{"name":"a"}"#,
        );
        let (flow, report) = build_flow_value(SaveInput {
            name: "login",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[login, me],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let text = serde_json::to_string(&flow).unwrap();
        assert!(!text.contains("hunter2"));
        assert!(report["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["reason"].as_str().unwrap().contains("secret")));
    }

    /// Whether a request carrying `body` would be kept if the recording were
    /// saved now: a request the recorder knows to carry a secret is dropped.
    fn survives_save(r: &mut FlowRecorder, body: &str) -> bool {
        r.begin_command(
            "click",
            &json!({ "action": "click", "selector": "#send" }),
            None,
            None,
        );
        r.tag_now("probe");
        let mut probe = entry(
            "probe",
            "https://api.example.com.ar/send",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        probe.method = "POST".to_string();
        probe.post_data = Some(body.to_string());
        build_flow(SaveInput {
            name: "probe",
            recorder: r,
            commands: &r.commands,
            description: None,
            entries: &[probe],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .is_ok()
    }

    fn assert_withheld(r: &mut FlowRecorder, secret: &str) {
        assert!(
            !survives_save(r, &format!(r#"{{"v":"{}"}}"#, secret)),
            "a request carrying {secret:?} was kept"
        );
    }

    fn assert_kept(r: &mut FlowRecorder, body: &str) {
        assert!(
            survives_save(r, &format!(r#"{{"v":"{}"}}"#, body)),
            "a request carrying {body:?} was dropped"
        );
    }

    fn recorded(steps: &[(&str, bool)]) -> FlowRecorder {
        let mut r = FlowRecorder::default();
        r.begin_capture(Some("https://www.example.com.ar/".to_string()));
        for (action, ok) in steps {
            let seq = r
                .begin_command(
                    action,
                    &json!({ "action": action, "url": "https://a.test/" }),
                    None,
                    None,
                )
                .unwrap();
            r.finish_command(seq, *ok, Some("https://a.test/".to_string()));
        }
        r
    }

    #[test]
    fn credential_headers_on_a_recorded_navigation_are_redacted_and_marked() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://a.test/", "headers": {
                "Authorization": "Bearer abc123", "Cookie": "sid=s3cret", "X-CSRF-Token": "tok9",
                "Accept-Language": "es" } }),
            None,
            None,
        );
        let cmd = &r.commands[0].1;
        let text = cmd.to_string();
        for secret in ["abc123", "s3cret", "tok9"] {
            assert!(!text.contains(secret), "{text}");
        }
        assert_eq!(cmd["headers"]["Authorization"], REDACTED);
        assert_eq!(cmd["headers"]["Accept-Language"], "es");
        assert_eq!(cmd["redacted"], true);
        // The values count as secrets, so a request that sends one is not saved.
        assert!(r.secrets.contains(&"Bearer abc123".to_string()));
        let flow = flow_of(
            json!({ "startUrl": "https://a.test/", "steps": [{ "command": cmd.clone() }] }),
        );
        assert!(replay_commands(&flow, &[]).unwrap_err().contains("secret"));
    }

    fn navigating_flow(start: &str, steps: Value, params: Value) -> Flow {
        flow_of(json!({ "startUrl": start, "params": params, "steps": steps }))
    }

    #[test]
    fn a_replayed_url_gets_the_value_encoded_for_its_query() {
        let flow = navigating_flow(
            "https://a.test/",
            json!([{ "command": { "action": "navigate", "url": "https://a.test/?q=kettle&x=1" } }]),
            json!([{ "name": "q", "example": "kettle" }]),
        );
        let cmds = replay_commands(&flow, &[("q".to_string(), "a&b#c d".to_string())]).unwrap();
        assert_eq!(cmds[0]["url"], "https://a.test/?q=a%26b%23c%20d&x=1");
    }

    #[test]
    fn each_swap_reads_the_recorded_text_and_never_an_earlier_result() {
        let flow = navigating_flow(
            "https://a.test/",
            json!([{ "command": { "action": "navigate", "url": "https://a.test/?from=Paris&to=London" } }]),
            json!([{ "name": "from", "example": "Paris" }, { "name": "to", "example": "London" }]),
        );
        let given = [
            ("from".to_string(), "London".to_string()),
            ("to".to_string(), "Rome".to_string()),
        ];
        let cmds = replay_commands(&flow, &given).unwrap();
        assert_eq!(cmds[0]["url"], "https://a.test/?from=London&to=Rome");
    }

    #[test]
    fn a_param_only_in_the_start_url_of_a_flow_on_an_open_page_is_replayed() {
        let flow = navigating_flow(
            "https://a.test/search?q=kettle",
            json!([{ "command": { "action": "click", "selector": "#go" } }]),
            json!([{ "name": "q", "example": "kettle" }]),
        );
        let cmds = replay_commands(&flow, &[("q".to_string(), "toaster".to_string())]).unwrap();
        assert_eq!(cmds[0]["action"], "navigate");
        assert_eq!(cmds[0]["url"], "https://a.test/search?q=toaster");
        assert_eq!(cmds[1]["action"], "click");
    }

    struct SilentBrowser;

    #[async_trait::async_trait]
    impl Browser for SilentBrowser {
        async fn cookie_header(&mut self, _: &str) -> Option<String> {
            None
        }
        async fn routed_response(&mut self, _: &str) -> Option<RoutedResponse> {
            None
        }
        async fn tab_url(&mut self) -> Option<String> {
            None
        }
        async fn eval(&mut self, _: &str) -> Result<Value, String> {
            Err("no page".to_string())
        }
        async fn ensure_browser(&mut self) -> Result<(), String> {
            Ok(())
        }
        async fn begin_capture(&mut self) -> Result<usize, String> {
            Ok(0)
        }
        async fn run(&mut self, _: &Value) -> Value {
            json!({ "success": true })
        }
        async fn captured_since(&mut self, _: usize) -> Vec<HarEntry> {
            Vec::new()
        }
        async fn end_capture(&mut self) {}
        fn write_approved(&self, _: &Write) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_write_the_replay_may_have_sent_without_a_captured_answer_is_unknown() {
        let flow = flow_of(json!({
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "command": { "action": "click", "selector": "#add" }, "requests": [
                { "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "body": "{}",
                  "expect": { "signature": signature(&json!({ "ok": true })) } }] }]
        }));
        let req = prepare_all(&flow, &[]).unwrap().remove(0);
        let ledger = WriteLedger::default();
        let ctx = SendCtx {
            ledger: &ledger,
            wanted: &[],
        };
        let mut run = None;
        let mut browser = SilentBrowser;
        let result = block_on(replay::send(&flow, &[], &mut browser, &mut run, &req, &ctx));
        assert!(!result.ok);
        assert_eq!(
            result.stop,
            Some(Stop::OutcomeUnknown),
            "{:?}",
            result.reason
        );
        // A read with no captured answer is still drift.
        let read = PreparedRequest {
            write: false,
            method: "GET".to_string(),
            ..req
        };
        let mut run = None;
        let result = block_on(replay::send(
            &flow,
            &[],
            &mut browser,
            &mut run,
            &read,
            &ctx,
        ));
        assert_eq!(result.stop, None);
        assert!(result.drift);
    }

    #[test]
    fn capture_accepts_only_flow() {
        assert!(parse_capture(&["flow".to_string()]).is_ok());
        let err = parse_capture(&["har".to_string()]).unwrap_err();
        assert!(
            err.contains("'har'") && err.contains("Supported: flow"),
            "{err}"
        );
    }

    #[test]
    fn a_save_starts_at_the_last_navigation_after_the_previous_save() {
        let mut r = recorded(&[
            ("navigate", true),
            ("click", true),
            ("navigate", true),
            ("click", true),
        ]);
        assert_eq!(r.resolve_range(None, None).unwrap(), (3, 4));
        assert_eq!(r.resolve_range(Some(2), Some(3)).unwrap(), (2, 3));
        r.last_save_seq = 4;
        assert_eq!(r.unsaved_steps(), 0);
        assert!(r.resolve_range(None, None).is_err());
        let seq = r
            .begin_command("click", &json!({ "action": "click" }), None, None)
            .unwrap();
        r.finish_command(seq, true, None);
        assert_eq!(r.resolve_range(None, None).unwrap(), (5, 5));
        assert_eq!(r.resolve_range(Some(5), None).unwrap(), (5, 5));
        assert_eq!(r.unsaved_steps(), 1);
        assert!(r.resolve_range(Some(9), None).is_err());
        assert!(r.resolve_range(None, Some(9)).is_err());
    }

    #[test]
    fn end_capture_reports_unsaved_steps_and_stops_recording() {
        let mut r = recorded(&[("navigate", true), ("click", true)]);
        r.last_save_seq = 1;
        assert_eq!(r.end_capture(), 1);
        assert!(!r.active());
        assert!(r.begin_command("click", &json!({}), None, None).is_none());
    }

    #[test]
    fn launch_commands_are_not_steps() {
        let mut r = recorded(&[]);
        assert!(r
            .begin_command("launch", &json!({ "capture": ["flow"] }), None, None)
            .is_none());
    }

    #[test]
    fn failed_steps_are_kept_and_flagged() {
        let r = recorded(&[("navigate", true), ("click", false)]);
        assert!(r.is_ok(1));
        assert!(!r.is_ok(2));
        assert_eq!(r.commands.len(), 2);
    }

    #[test]
    fn a_suspended_recorder_records_nothing() {
        let mut r = recorded(&[("navigate", true)]);
        r.set_suspended(true);
        assert!(r.begin_command("click", &json!({}), None, None).is_none());
        r.tag_now("late");
        assert!(r.request_tags.is_empty());
    }

    #[test]
    fn replay_skips_failed_steps() {
        let flow = flow_of(json!({
            "params": [],
            "startUrl": "https://a.test/",
            "steps": [
                { "ok": true, "command": { "action": "navigate", "url": "https://a.test/" } },
                { "ok": false, "command": { "action": "click", "selector": "#gone" } },
                { "ok": true, "command": { "action": "click", "selector": "#go" } }
            ]
        }));
        let cmds = replay_commands(&flow, &[]).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[1]["selector"], "#go");
    }

    #[test]
    fn a_flow_file_with_an_unknown_field_or_version_is_refused() {
        let dir = std::env::temp_dir().join(format!("ab-flow-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.to_str().unwrap());
        let mut flow =
            flow_of(json!({ "name": "f", "origin": "a.test", "startUrl": "https://a.test/" }));
        write_flow(&flow).unwrap();
        assert!(load_flow("f", None).is_ok());
        let path = dir.join("a.test").join("f.json");
        let mut text: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        text["surprise"] = json!(true);
        std::fs::write(&path, serde_json::to_string(&text).unwrap()).unwrap();
        let err = load_flow("f", None).unwrap_err();
        assert!(err.contains("surprise"), "{err}");
        flow.version = FLOW_VERSION + 1;
        write_flow(&flow).unwrap();
        assert!(load_flow("f", None)
            .unwrap_err()
            .contains("unsupported version"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn origins_must_be_plain_hostnames() {
        for ok in ["example.com", "127.0.0.1", "my-site.co.uk", "localhost"] {
            assert!(validate_origin(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "..", "a/b", "../x", "a..b", ".hidden", "a\\b", "a b", "x.com/",
        ] {
            assert!(validate_origin(bad).is_err(), "{bad}");
        }
        let flow = flow_of(json!({ "version": 1, "name": "x", "origin": "../escape" }));
        assert!(write_flow(&flow)
            .unwrap_err()
            .contains("invalid flow origin"));
        assert!(flow_path("a/b", "x").is_err());
    }

    #[test]
    fn mutating_segments_match_whole_segments_only() {
        for yes in [
            "https://a.test/logout",
            "https://a.test/account/signout?x=1",
            "https://a.test/api/items/delete",
            "https://a.test/Unsubscribe/abc",
            "https://a.test/cart/remove",
        ] {
            assert!(has_mutating_segment(yes), "{yes}");
        }
        for no in [
            "https://a.test/address",
            "https://a.test/deleted",
            "https://a.test/api/removed-items",
            "https://a.test/logout-help.html",
            "https://a.test/search?q=logout",
        ] {
            assert!(!has_mutating_segment(no), "{no}");
        }
    }

    fn writes_after(first: &str, entries: Vec<HarEntry>, tag: &[&str]) -> Value {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.begin_command(
            "click",
            &json!({ "action": first, "selector": "#b" }),
            None,
            None,
        );
        for t in tag {
            r.tag_now(t);
        }
        build_flow_value(SaveInput {
            name: "w",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .map(|(f, _)| f["writes"].clone())
        .unwrap_or_else(|e| json!(e))
    }

    #[test]
    fn a_get_a_click_fired_is_a_write_only_when_its_path_says_so() {
        // (path, suspected)
        for (path, suspected) in [
            ("/cart/add?id=7", false),
            ("/cart/remove?id=7", true),
            ("/account/logout", true),
            ("/address/list", false),
            ("/api/items", false),
        ] {
            let call = entry(
                "call",
                &format!("https://www.example.com.ar{}", path),
                "Fetch",
                "application/json",
                r#"{"ok":true}"#,
            );
            let writes = writes_after("click", vec![call], &["call"]);
            let writes = writes.as_array().unwrap();
            assert_eq!(writes.len(), usize::from(suspected), "{path}: {writes:?}");
            if suspected {
                assert_eq!(writes[0]["kind"], "suspected");
                assert_eq!(writes[0]["method"], "GET");
            }
        }
    }

    #[test]
    fn a_get_fired_by_navigation_is_a_read_unless_its_path_says_otherwise() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        for t in ["data", "out"] {
            r.tag_now(t);
        }
        let entries = vec![
            entry(
                "data",
                "https://api.example.com.ar/other",
                "Fetch",
                "application/json",
                r#"{"a":[1]}"#,
            ),
            entry(
                "out",
                "https://www.example.com.ar/logout",
                "Fetch",
                "application/json",
                r#"{"ok":true}"#,
            ),
        ];
        let (flow, _) = build_flow_value(SaveInput {
            name: "n",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: Some("s1r1"),
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let writes = flow["writes"].as_array().unwrap();
        assert_eq!(writes.len(), 1);
        assert!(writes[0]["url"].as_str().unwrap().ends_with("/logout"));
        assert_eq!(writes[0]["kind"], "suspected");
    }

    #[test]
    fn allow_write_takes_all_or_ids() {
        assert_eq!(AllowWrite::parse(None).unwrap(), AllowWrite::default());
        assert!(AllowWrite::parse(Some(&json!("all")))
            .unwrap()
            .covers("anything"));
        let some = AllowWrite::parse(Some(&json!("s1r2, s3w1"))).unwrap();
        assert!(some.covers("s1r2") && some.covers("s3w1") && !some.covers("s1r1"));
        assert!(AllowWrite::parse(Some(&json!(""))).is_err());
        assert!(AllowWrite::parse(Some(&json!(true))).is_err());
        let flow =
            flow_of(json!({ "writes": [{ "id": "s1r2", "method": "POST", "kind": "explicit" }] }));
        assert!(some.check(&flow).unwrap_err().contains("s3w1"));
    }

    fn traits(input_type: &str, autocomplete: &str, hint: &str) -> FieldTraits {
        FieldTraits {
            input_type: input_type.to_string(),
            autocomplete: autocomplete.to_string(),
            hint: hint.to_string(),
        }
    }

    #[test]
    fn a_typed_email_becomes_a_param_and_no_value_stays_literal() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "@e2", "value": "ana@example.com" }),
            None,
            Some(&traits("email", "username", "email Email address")),
        );
        r.tag_now("lookup");
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "@e3", "value": "482913" }),
            None,
            Some(&traits("text", "one-time-code", "")),
        );
        assert_eq!(r.commands[1].1["value"], "{{email}}");
        let mut lookup = entry(
            "lookup",
            "https://api.example.com.ar/lookup",
            "Fetch",
            "application/json",
            r#"{"found":true}"#,
        );
        lookup.method = "POST".to_string();
        lookup.post_data = Some(r#"{"email":"ana@example.com"}"#.to_string());
        let (flow, _) = build_flow_value(SaveInput {
            name: "t",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[lookup],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let param = flow["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "email")
            .unwrap();
        assert_eq!(param["source"], "typed");
        assert!(param.get("example").is_none(), "{param}");
        assert_eq!(flow["steps"][1]["command"]["value"], "{{email}}");
        assert_eq!(
            flow["steps"][1]["requests"][0]["body"],
            r#"{"email":"{{email}}"}"#
        );
        assert_eq!(flow["steps"][2]["command"]["value"], REDACTED);
        assert_eq!(flow["redactedSteps"], json!([3]));
        let text = serde_json::to_string(&flow).unwrap();
        assert!(!text.contains("482913"));
        // The step holds no literal; only the param keeps the recorded example.
        assert_eq!(text.matches("ana@example.com").count(), 0);
    }

    #[test]
    fn a_param_flag_names_the_typed_value() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "#q", "value": "kettle" }),
            None,
            Some(&traits("text", "", "q")),
        );
        r.tag_now("search");
        let entries = vec![entry(
            "search",
            "https://api.example.com.ar/s?q=kettle",
            "Fetch",
            "application/json",
            r#"{"items":[{"id":1}]}"#,
        )];
        let params = vec![("product".to_string(), "kettle".to_string())];
        let (flow, _) = build_flow_value(SaveInput {
            name: "t",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &params,
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        assert_eq!(flow["steps"][1]["command"]["value"], "{{product}}");
        let names: Vec<&str> = flow["params"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect();
        assert_eq!(names, vec!["product"]);
        assert_eq!(
            flow["steps"][1]["requests"][0]["url"],
            "https://api.example.com.ar/s?q={{product}}"
        );
    }

    #[test]
    fn replay_fills_typed_placeholders_with_the_run_value_or_the_example() {
        let flow = flow_of(json!({
            "startUrl": "https://a.test/",
            "params": [{ "name": "email", "example": "ana@example.com", "source": "typed" }],
            "steps": [
                { "ok": true, "command": { "action": "navigate", "url": "https://a.test/" } },
                { "ok": true, "command": { "action": "fill", "selector": "#e", "value": "{{email}}" } }
            ]
        }));
        let given = replay_commands(
            &flow,
            &[("email".to_string(), "bo@example.com".to_string())],
        )
        .unwrap();
        assert_eq!(given[1]["value"], "bo@example.com");
        let recorded_value = replay_commands(&flow, &[]).unwrap();
        assert_eq!(recorded_value[1]["value"], "ana@example.com");
    }

    #[test]
    fn typed_param_names_follow_the_field_and_stay_unique() {
        let t = traits("text", "", "Search query");
        assert_eq!(typed_param_name(Some(&t), &[]), "search");
        assert_eq!(
            typed_param_name(Some(&t), &["search".to_string()]),
            "search2"
        );
        assert_eq!(typed_param_name(None, &[]), "typed");
        assert_eq!(
            typed_param_name(Some(&traits("text", "", "123")), &[]),
            "typed"
        );
    }

    #[test]
    fn retry_after_is_kept_only_for_rate_limits() {
        let req = prepared("s1r1");
        let limited = tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Err(Failure::stop(Stop::RateLimited, 429, "x".into())),
        );
        assert_eq!(
            with_retry_after(limited, Some("30".into()))
                .retry_after
                .as_deref(),
            Some("30")
        );
        let auth = tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Err(Failure::stop(Stop::AuthExpired, 401, "x".into())),
        );
        assert_eq!(with_retry_after(auth, Some("30".into())).retry_after, None);
    }

    #[test]
    fn a_run_gates_only_the_writes_its_output_needs() {
        let request = |id: &str, url: &str, body: &str| json!({ "id": id, "method": "POST", "url": url, "body": body });
        let writes = json!([
            { "id": "s1r1", "method": "POST", "url": "https://a.test/session", "kind": "explicit" },
            { "id": "s2r1", "method": "POST", "url": "https://a.test/cart", "kind": "explicit" },
            { "id": "s3w1", "method": "GET", "url": "https://a.test/x", "kind": "suspected" }
        ]);
        let flow = flow_of(json!({
            "output": "s2r1", "writes": writes,
            "steps": [
                { "command": {}, "requests": [request("s1r1", "https://a.test/session", "{}")] },
                { "command": {}, "requests": [request("s2r1", "https://a.test/cart", "{\"csrf\":\"{{@s1r1.csrf}}\"}")] }
            ]
        }));
        assert_eq!(write_ids(&flow), vec!["s1r1", "s2r1"]);
        let only_output = flow_of(json!({
            "output": "s2r1", "writes": writes,
            "steps": [
                { "command": {}, "requests": [request("s1r1", "https://a.test/session", "{}")] },
                { "command": {}, "requests": [request("s2r1", "https://a.test/cart", "{}")] }
            ]
        }));
        assert_eq!(write_ids(&only_output), vec!["s2r1"]);
        let replay: Vec<&str> = only_output
            .replay_only_writes()
            .iter()
            .map(|w| w.id.as_str())
            .collect();
        assert_eq!(replay, vec!["s1r1", "s3w1"]);
    }

    fn denied(id: &str, tier: Tier, status: u16) -> TierResult {
        let req = PreparedRequest {
            expect: expect_of(json!({ "signature": signature(&json!({})), "strippedAuth": true })),
            ..prepared(id)
        };
        tier_result(
            &req,
            tier,
            Instant::now(),
            Err(Failure::stop(
                Stop::CredentialNotSendable,
                status,
                "x".into(),
            )),
        )
    }

    #[test]
    fn a_401_on_a_request_recorded_with_credentials_falls_through() {
        let http = denied("s1r1", Tier::Http, 401);
        assert_eq!(http.stop, None);
        assert!(http.drift);
        let page = denied("s1r1", Tier::Page, 403);
        assert_eq!(page.stop, None);
        let replay = denied("s1r1", Tier::Replay, 401);
        assert_eq!(replay.stop, Some(Stop::CredentialNotSendable));
        // Without stripped credentials the first 401 still stops the run.
        let req = prepared("s1r1");
        let plain = tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Err(Failure::stop(Stop::AuthExpired, 401, "x".into())),
        );
        assert_eq!(plain.stop, Some(Stop::AuthExpired));
        // When no way gets through, the run reports that the credential cannot be sent.
        let all = vec![
            denied("s1r1", Tier::Http, 401),
            denied("s1r1", Tier::Page, 403),
        ];
        assert!(denied_everywhere(&all).is_some());
        let recovered = vec![
            denied("s1r1", Tier::Http, 401),
            attempt(
                "s1r1",
                Tier::Page,
                Ok((200, r#"{"data":[{"id":1}]}"#.to_string())),
            ),
        ];
        assert!(denied_everywhere(&recovered).is_none());
    }

    #[test]
    fn save_records_whether_credential_headers_were_stripped() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_now("api");
        let mut api = entry(
            "api",
            "https://api.example.com.ar/items",
            "Fetch",
            "application/json",
            r#"{"items":[{"id":1}]}"#,
        );
        api.request_headers = vec![
            ("Authorization".into(), "Bearer abc".into()),
            ("Accept".into(), "*/*".into()),
        ];
        let (flow, _) = build_flow_value(SaveInput {
            name: "t",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[api],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        assert_eq!(
            flow["steps"][0]["requests"][0]["expect"]["strippedAuth"],
            true
        );
        assert!(!serde_json::to_string(&flow).unwrap().contains("Bearer"));
    }

    #[test]
    fn replay_runs_only_plain_interactions_on_the_flows_site() {
        let flow =
            flow_of(json!({ "startUrl": "https://example.com/", "allowedHosts": ["example.com"] }));
        let ok = vec![
            json!({ "action": "navigate", "url": "https://example.com/a" }),
            json!({ "action": "click", "selector": "a[href='https://other.test']" }),
            json!({ "action": "tab_new", "url": "https://www.example.com/" }),
        ];
        assert!(check_replay_commands(&flow, &ok).is_ok());
        for bad in [
            json!({ "action": "evaluate", "script": "fetch('https://evil.test')" }),
            json!({ "action": "tab_new", "url": "https://evil.test/" }),
            json!({ "action": "window_new", "url": "https://example.com/" }),
            json!({ "action": "click", "selector": "x", "extra": { "url": "https://evil.test" } }),
            json!({ "action": "navigate", "url": "javascript:alert(1)" }),
            json!({ "action": "click", "script": "fetch('/x')" }),
        ] {
            assert!(
                check_replay_commands(&flow, std::slice::from_ref(&bad)).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_flow_whose_origin_does_not_match_its_directory_or_start_url_is_refused() {
        let dir = std::env::temp_dir().join(format!("ab-flow-origin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.to_str().unwrap());
        let good = flow_of(
            json!({ "version": 1, "name": "f", "origin": "a.test", "startUrl": "https://a.test/", "steps": [] }),
        );
        write_flow(&good).unwrap();
        assert!(load_flow("f", None).is_ok());
        // Moved under another origin's directory, or start URL edited.
        let moved = dir.join("b.test");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::copy(dir.join("a.test").join("f.json"), moved.join("g.json")).unwrap();
        let err = load_flow("f", Some("b.test")).unwrap_err();
        assert!(
            err.contains("re-save") || err.contains("no saved flow"),
            "{err}"
        );
        std::fs::remove_dir_all(&moved).unwrap();
        let mut edited = good.clone();
        edited.start_url = "https://evil.test/".to_string();
        std::fs::write(
            dir.join("a.test").join("f.json"),
            serde_json::to_string(&edited).unwrap(),
        )
        .unwrap();
        assert!(load_flow("f", None).unwrap_err().contains("re-save"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_marker_never_moves_back_when_an_earlier_range_is_saved() {
        let mut r = recorded(&[("navigate", true), ("click", true), ("click", true)]);
        r.mark_saved(3);
        r.mark_saved(1);
        assert_eq!(r.last_save_seq, 3);
        assert_eq!(r.unsaved_steps(), 0);
    }

    #[test]
    fn short_secrets_are_redacted_and_short_plain_values_are_not_templated() {
        for kind in ["fill", "inserttext"] {
            let mut r = recorded(&[]);
            let cmd = if kind == "fill" {
                json!({ "action": "fill", "selector": "@e1", "value": "42" })
            } else {
                json!({ "action": "inserttext", "text": "42" })
            };
            r.begin_command(kind, &cmd, None, Some(&traits("text", "one-time-code", "")));
            let c = &r.commands[0].1;
            assert!(c["value"] == REDACTED || c["text"] == REDACTED, "{c}");
            assert_withheld(&mut r, "42");
        }
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "#q", "value": "ab" }),
            None,
            Some(&traits("text", "", "q")),
        );
        r.tag_now("s");
        let entries = vec![entry(
            "s",
            "https://api.example.com.ar/s?q=ab",
            "Fetch",
            "application/json",
            r#"{"items":[{"id":1}]}"#,
        )];
        let (flow, _) = build_flow_value(SaveInput {
            name: "t",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &entries,
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        assert_eq!(flow["steps"][1]["command"]["value"], "{{q}}");
        assert_eq!(
            flow["steps"][1]["requests"][0]["url"],
            "https://api.example.com.ar/s?q=ab"
        );
    }

    #[test]
    fn keyboard_and_inserttext_are_typed_fields_and_late_traits_redact() {
        let mut r = recorded(&[]);
        let seq = r
            .begin_command(
                "keyboard",
                &json!({ "action": "keyboard", "subaction": "type", "text": "s3cret!" }),
                None,
                None,
            )
            .unwrap();
        assert_eq!(r.commands[0].1["text"], "{{typed}}");
        assert_eq!(r.needs_traits, Some(seq));
        r.redact_if_secret(seq, &traits("password", "", ""));
        assert_eq!(r.commands[0].1["text"], REDACTED);
        assert_withheld(&mut r, "s3cret!");
        let mut r = recorded(&[]);
        r.begin_command(
            "inserttext",
            &json!({ "action": "inserttext", "text": "plain" }),
            None,
            Some(&traits("text", "", "note")),
        );
        assert_eq!(r.commands[0].1["text"], "{{note}}");
    }

    #[test]
    fn typed_params_are_required_at_run_time_and_listed_without_a_value() {
        let flow = flow_of(json!({
            "startUrl": "https://a.test/",
            "params": [{ "name": "email", "source": "typed" }],
            "steps": [{ "ok": true, "command": { "action": "fill", "selector": "#e", "value": "{{email}}" }, "requests": [] }]
        }));
        assert!(replay_commands(&flow, &[])
            .unwrap_err()
            .contains("missing --param email"));
        let cmds = replay_commands(&flow, &[("email".to_string(), "x@y.z".to_string())]).unwrap();
        assert_eq!(cmds[1]["value"], "x@y.z");
        assert!(input_schema(&flow)["properties"]["email"]["description"]
            .as_str()
            .unwrap()
            .contains("pass a value"));
    }

    #[test]
    fn urls_hidden_by_whatwg_stripping_are_still_host_checked() {
        let flow =
            flow_of(json!({ "startUrl": "https://example.com/", "allowedHosts": ["example.com"] }));
        for bad in [
            "ht\ttps://evil.com/x",
            "\u{1}https://evil.com/x",
            " https://evil.com/x",
            "https://evil.com\n/x",
            "java\tscript:alert(1)",
        ] {
            let cmd = json!({ "action": "tab_new", "url": bad });
            assert!(check_replay_commands(&flow, &[cmd]).is_err(), "{bad:?}");
            let nested = json!({ "action": "click", "selector": "x", "arg": bad });
            assert!(check_replay_commands(&flow, &[nested]).is_err(), "{bad:?}");
        }
        // A URL key is checked even when the value is not an http URL.
        let odd = json!({ "action": "tab_new", "url": "chrome://settings" });
        assert!(check_replay_commands(&flow, &[odd]).is_err());
        let fine = json!({ "action": "tab_new", "url": "/relative/path" });
        assert!(check_replay_commands(&flow, &[fine]).is_ok());
        let blank = json!({ "action": "navigate", "url": "about:blank" });
        assert!(check_replay_commands(&flow, &[blank]).is_ok());
    }

    #[test]
    fn a_write_is_never_retried_by_the_in_page_fetch() {
        let write = PreparedRequest {
            method: "POST".to_string(),
            expect: Expect::default(),
            write: true,
            ..prepared("s1r1")
        };
        let script = page_fetch_script(&write);
        assert!(!script.contains("catch"), "{script}");
        assert_eq!(script.matches("fetch(").count(), 1, "{script}");
        let read = page_fetch_script(&prepared("s1r1"));
        assert!(read.contains("catch"));
    }

    #[test]
    fn a_command_that_is_not_recorded_stops_tagging_requests_to_the_last_step() {
        let mut r = recorded(&[("navigate", true)]);
        r.tag_now("belongs-to-step-1");
        r.untag_current();
        r.tag_now("from-an-unrecorded-command");
        assert_eq!(r.request_tags.len(), 1);
        assert!(r.request_tags.contains_key("belongs-to-step-1"));
    }

    #[test]
    fn keys_pressed_in_a_secret_field_are_gathered_as_one_secret() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        for key in ["p", "a", "x", "Backspace", "s", "s"] {
            r.begin_command(
                "press",
                &json!({ "action": "press", "key": key }),
                None,
                Some(&secret),
            );
        }
        for (i, key) in ["p", "a", "x", "s", "s"].iter().enumerate() {
            let idx = [0, 1, 2, 4, 5][i];
            assert_eq!(r.commands[idx].1["key"], REDACTED, "{key}");
            assert_eq!(r.commands[idx].1["redacted"], true);
        }
        assert_eq!(r.commands[3].1["key"], "Backspace");
        // Enter ends the password and is kept as a step.
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Enter" }),
            None,
            Some(&secret),
        );
        assert_eq!(r.commands[6].1["key"], "Enter");
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "z" }),
            None,
            Some(&secret),
        );
        assert_withheld(&mut r, "pass");
        assert_withheld(&mut r, "z");
        assert_kept(&mut r, "pax");
        // Keys in an ordinary field stay as they are.
        let mut plain = recorded(&[]);
        plain.begin_command(
            "press",
            &json!({ "action": "press", "key": "q" }),
            None,
            Some(&traits("text", "", "search")),
        );
        assert_eq!(plain.commands[0].1["key"], "q");
        assert_kept(&mut plain, "q");
        // Shift combos and keydown count as characters.
        let mut r = recorded(&[]);
        r.begin_command(
            "keydown",
            &json!({ "action": "keydown", "key": "Shift+A" }),
            None,
            Some(&secret),
        );
        r.begin_command(
            "keyup",
            &json!({ "action": "keyup", "key": "Shift+A" }),
            None,
            Some(&secret),
        );
        assert_withheld(&mut r, "A");
        assert_eq!(r.commands[1].1["key"], REDACTED);
    }

    #[test]
    fn a_request_carrying_a_key_by_key_password_is_not_saved() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        for key in "hunter2!".chars() {
            r.begin_command(
                "press",
                &json!({ "action": "press", "key": key.to_string() }),
                None,
                Some(&secret),
            );
        }
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Enter" }),
            None,
            Some(&secret),
        );
        r.tag_now("login");
        r.tag_now("me");
        let mut login = entry(
            "login",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        login.method = "POST".to_string();
        login.post_data = Some(r#"{"password":"hunter2!"}"#.to_string());
        let me = entry(
            "me",
            "https://api.example.com.ar/me",
            "Fetch",
            "application/json",
            r#"{"name":"a"}"#,
        );
        let (flow, _) = build_flow_value(SaveInput {
            name: "k",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[login, me],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let text = serde_json::to_string(&flow).unwrap();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(
            !text.contains("api.example.com.ar/login"),
            "the login request is dropped"
        );
        assert_eq!(flow["steps"][1]["command"]["key"], REDACTED);
    }

    fn secret_fill_recording() -> (FlowRecorder, HarEntry) {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/vault" }),
            None,
            None,
        );
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "@e1", "value": "tk-91ab-77" }),
            None,
            Some(&traits("text", "", "secret value")),
        );
        r.tag_now("save");
        let mut save = entry(
            "save",
            "https://api.example.com.ar/api/vault/items?v=2",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        save.method = "POST".to_string();
        save.post_data = Some(r#"{"label":"a","value":"tk-91ab-77"}"#.to_string());
        (r, save)
    }

    #[test]
    fn a_save_that_drops_the_only_request_for_a_secret_names_it() {
        let (r, save) = secret_fill_recording();
        let err = build_flow_value(SaveInput {
            name: "vault",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[save],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap_err();
        assert!(err.contains("POST /api/vault/items"), "{err}");
        assert!(err.contains("typed into a secret field"), "{err}");
        assert!(err.contains("non-secret field"), "{err}");
        assert!(!err.contains("tk-91ab-77") && !err.contains("v=2"), "{err}");
    }

    #[test]
    fn a_dropped_secret_request_is_named_in_the_not_captured_line() {
        let (r, save) = secret_fill_recording();
        let mut r = r;
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_now("home");
        let home = entry(
            "home",
            "https://api.example.com.ar/home",
            "Fetch",
            "application/json",
            r#"{"items":[1]}"#,
        );
        let (_, report) = build_flow_value(SaveInput {
            name: "vault",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[save, home],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let reasons: Vec<String> = report["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| g["reason"].as_str().map(String::from))
            .collect();
        assert!(
            reasons
                .iter()
                .any(|m| m.contains("POST /api/vault/items") && m.contains("secret field")),
            "{reasons:?}"
        );
    }

    #[test]
    fn a_secret_in_the_path_of_a_dropped_request_is_not_printed() {
        for url in [
            "https://api.example.com.ar/api/vault/tk-91ab-77?v=2",
            "https://api.example.com.ar/api/vault/tk%2D91ab%2D77/items",
        ] {
            let (r, mut save) = secret_fill_recording();
            save.url = url.to_string();
            save.post_data = Some(r#"{"label":"a"}"#.to_string());
            let err = build_flow_value(SaveInput {
                name: "vault",
                recorder: &r,
                commands: &r.commands,
                description: None,
                entries: &[save.clone()],
                params: &[],
                output: None,
                created_at: String::new(),
                login_wall: false,
            })
            .unwrap_err();
            assert!(!err.contains("91ab"), "{err}");
            assert!(err.contains("POST /api/vault/<redacted>"), "{err}");
            let mut r = r;
            r.begin_command(
                "navigate",
                &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
                None,
                None,
            );
            r.tag_now("home");
            let home = entry(
                "home",
                "https://api.example.com.ar/home",
                "Fetch",
                "application/json",
                r#"{"items":[1]}"#,
            );
            let (_, mut report) = build_flow_value(SaveInput {
                name: "vault",
                recorder: &r,
                commands: &r.commands,
                description: None,
                entries: &[save, home],
                params: &[],
                output: None,
                created_at: String::new(),
                login_wall: false,
            })
            .unwrap();
            assert!(!report["gaps"].to_string().contains("91ab"), "{report}");
            report["path"] = json!("/tmp/x.json");
            let text =
                crate::output::format_flow_text(Some("flow_save"), &report).unwrap_or_default();
            assert!(!text.contains("91ab"), "{text}");
        }
    }

    #[test]
    fn a_secret_that_spans_path_segments_is_not_printed() {
        let mut r = recorded(&[]);
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "@e1", "value": "ab12/cd34" }),
            None,
            Some(&traits("text", "", "secret value")),
        );
        r.tag_now("save");
        let mut save = entry(
            "save",
            "https://api.example.com.ar/api/vault/ab12/cd34/items",
            "Fetch",
            "application/json",
            "{}",
        );
        save.method = "POST".to_string();
        let err = build_flow_value(SaveInput {
            name: "vault",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[save],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap_err();
        assert!(!err.contains("ab12") && !err.contains("cd34"), "{err}");
        assert!(err.contains("POST api.example.com.ar carried"), "{err}");
    }

    #[test]
    fn an_unreadable_paste_names_only_the_host_of_the_dropped_request() {
        let mut r = recorded(&[]);
        let secret = traits("password", "", "");
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Control+v" }),
            None,
            Some(&secret),
        );
        r.tag_now("save");
        let mut save = entry(
            "save",
            "https://api.example.com.ar/api/vault/pasted-9z/items?v=2",
            "Fetch",
            "application/json",
            "{}",
        );
        save.method = "POST".to_string();
        let err = build_flow_value(SaveInput {
            name: "vault",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[save],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap_err();
        assert!(err.contains("POST api.example.com.ar may carry"), "{err}");
        assert!(!err.contains("pasted-9z"), "{err}");
    }

    #[test]
    fn modifier_chords_do_not_join_the_secret_buffer() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Control+a" }),
            None,
            Some(&secret),
        );
        for ch in "hunter2".chars() {
            r.begin_command(
                "press",
                &json!({ "action": "press", "key": ch.to_string() }),
                None,
                Some(&secret),
            );
        }
        assert_withheld(&mut r, "hunter2");
        let mut r = recorded(&[]);
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Meta+v" }),
            None,
            Some(&secret),
        );
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "x" }),
            None,
            Some(&secret),
        );
        assert_withheld(&mut r, "x");
        // Shift is a plain character modifier.
        let mut r = recorded(&[]);
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Shift+H" }),
            None,
            Some(&secret),
        );
        assert_withheld(&mut r, "H");
    }

    #[test]
    fn a_paste_into_a_secret_field_redacts_the_step_and_drops_what_it_tagged() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Control+v" }),
            None,
            Some(&secret),
        );
        r.tag_now("login");
        r.tag_now("me");
        let mut login = entry(
            "login",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        login.method = "POST".to_string();
        login.post_data = Some(r#"{"password":"pasted-value"}"#.to_string());
        let me = entry(
            "me",
            "https://api.example.com.ar/me",
            "Fetch",
            "application/json",
            r#"{"name":"a"}"#,
        );
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_now("home");
        let home = entry(
            "home",
            "https://api.example.com.ar/home",
            "Fetch",
            "application/json",
            r#"{"items":[1]}"#,
        );
        let (flow, report) = build_flow_value(SaveInput {
            name: "p",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[login, me, home],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let text = serde_json::to_string(&flow).unwrap();
        assert!(!text.contains("pasted-value"));
        assert_eq!(flow["steps"][1]["command"]["key"], REDACTED);
        assert_eq!(flow["steps"][1]["requests"], json!([]));
        assert!(
            report["gaps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["step"] == 2),
            "{report}"
        );
        assert_eq!(flow["steps"][2]["requests"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn clipboard_write_text_is_a_secret() {
        let mut r = recorded(&[]);
        r.begin_command(
            "clipboard",
            &json!({ "action": "clipboard", "operation": "write", "text": "s3cret-token" }),
            None,
            None,
        );
        assert_eq!(r.commands[0].1["text"], REDACTED);
        assert_eq!(r.commands[0].1["redacted"], true);
        assert_withheld(&mut r, "s3cret-token");
        r.begin_command(
            "clipboard",
            &json!({ "action": "clipboard", "operation": "read" }),
            None,
            None,
        );
        assert!(r.commands[1].1.get("redacted").is_none());
    }

    #[test]
    fn a_pasted_password_is_registered_so_a_later_submit_does_not_save_it() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        let paste = r
            .begin_command(
                "press",
                &json!({ "action": "press", "key": "Control+v" }),
                None,
                Some(&secret),
            )
            .unwrap();
        assert_eq!(r.paste_pending, Some(paste));
        r.resolve_paste(paste, Some("pasted-value".to_string()));
        assert_withheld(&mut r, "pasted-value");
        assert!(
            r.unknown_secret_seqs.is_empty(),
            "the value was read, so no fallback"
        );
        r.begin_command(
            "click",
            &json!({ "action": "click", "selector": "#submit" }),
            None,
            None,
        );
        r.tag_now("login");
        r.tag_now("me");
        let mut login = entry(
            "login",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        login.method = "POST".to_string();
        login.post_data = Some(r#"{"password":"pasted-value"}"#.to_string());
        let me = entry(
            "me",
            "https://api.example.com.ar/me",
            "Fetch",
            "application/json",
            r#"{"name":"a"}"#,
        );
        let (flow, _) = build_flow_value(SaveInput {
            name: "p",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[login, me],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let text = serde_json::to_string(&flow).unwrap();
        assert!(!text.contains("pasted-value"), "{text}");
        assert!(!text.contains("api.example.com.ar/login"), "{text}");
    }

    #[test]
    fn a_paste_with_a_prefix_registers_the_full_value_and_keeps_extending() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "a" }),
            None,
            Some(&secret),
        );
        let paste = r
            .begin_command(
                "press",
                &json!({ "action": "press", "key": "Meta+v" }),
                None,
                Some(&secret),
            )
            .unwrap();
        r.resolve_paste(paste, Some("apasted".to_string()));
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "!" }),
            None,
            Some(&secret),
        );
        assert_withheld(&mut r, "apasted!");
    }

    #[test]
    fn an_unreadable_paste_keeps_the_fallback() {
        let secret = traits("password", "", "");
        let mut r = recorded(&[]);
        let paste = r
            .begin_command(
                "press",
                &json!({ "action": "press", "key": "Control+v" }),
                None,
                Some(&secret),
            )
            .unwrap();
        r.resolve_paste(paste, None);
        assert_eq!(r.unknown_secret_seqs, vec![paste]);
        assert_eq!(r.paste_pending, None);
    }

    // ctate review comments, reproduced against the saved-flow code.

    #[test]
    fn a_param_with_quotes_keeps_commands_bodies_urls_and_headers_valid() {
        let value = r#"a "quoted" item\"#.to_string();
        let flow = flow_of(json!({
            "startUrl": "https://a.test/",
            "params": [{ "name": "q", "source": "typed" }],
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "ok": true, "command": { "action": "fill", "selector": "#q", "value": "{{q}}" },
                "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/s?q={{q}}",
                    "headers": { "x-q": "{{q}}" }, "body": "{\"q\":\"{{q}}\"}",
                    "expect": { "signature": signature(&json!({})) } }] }]
        }));
        let cmds = replay_commands(&flow, &[("q".to_string(), value.clone())]).unwrap();
        assert_eq!(cmds[1]["value"], value.as_str());
        assert!(serde_json::from_str::<Value>(&serde_json::to_string(&cmds[1]).unwrap()).is_ok());
        let prepared = prepare_all(&flow, &[("q".to_string(), value.clone())]).unwrap();
        let body: Value = serde_json::from_str(prepared[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["q"], value.as_str());
        assert!(prepared[0].url.contains("%22quoted%22"));
        assert_eq!(prepared[0].headers[0].1, value);
    }

    #[test]
    fn type_into_a_password_selector_is_redacted() {
        let mut r = recorded(&[]);
        r.begin_command(
            "type",
            &json!({ "action": "type", "selector": "#password", "text": "secret" }),
            None,
            None,
        );
        assert_eq!(r.commands[0].1["text"], REDACTED);
        assert_withheld(&mut r, "secret");
    }

    #[test]
    fn a_json_login_carrying_a_filled_password_is_not_saved() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/login" }),
            None,
            None,
        );
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "#password", "value": "hunter2" }),
            None,
            None,
        );
        r.begin_command(
            "click",
            &json!({ "action": "click", "selector": "#go" }),
            None,
            None,
        );
        r.tag_now("login");
        r.tag_now("me");
        let mut login = entry(
            "login",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            r#"{"ok":true}"#,
        );
        login.method = "POST".to_string();
        login.post_data = Some(r#"{"user":"a","password":"hunter2"}"#.to_string());
        let me = entry(
            "me",
            "https://api.example.com.ar/me",
            "Fetch",
            "application/json",
            r#"{"name":"a"}"#,
        );
        let (flow, _) = build_flow_value(SaveInput {
            name: "l",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[login, me],
            params: &[],
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let text = serde_json::to_string(&flow).unwrap();
        assert!(
            !text.contains("hunter2") && !text.contains("api.example.com.ar/login"),
            "{text}"
        );
    }

    #[test]
    fn a_secret_is_recognized_in_form_percent_and_json_encodings() {
        let secret = vec!["hunter two".to_string(), "p&ss/w=rd+1".to_string()];
        let mut form = entry(
            "a",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            "{}",
        );
        form.post_data = Some("user=a&password=hunter+two".to_string());
        assert!(carries_secret(&form, &secret), "plus-encoded form body");
        form.post_data = Some("user=a&password=hunter%20two".to_string());
        assert!(carries_secret(&form, &secret), "percent-encoded form body");
        let query = entry(
            "b",
            "https://api.example.com.ar/login?password=hunter+two",
            "Fetch",
            "application/json",
            "{}",
        );
        assert!(carries_secret(&query, &secret), "plus-encoded query");
        let mut mixed = entry(
            "c",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            "{}",
        );
        mixed.post_data = Some("password=p%26ss%2Fw%3Drd%2B1".to_string());
        assert!(carries_secret(&mixed, &secret), "reserved characters");
        let mut json_escaped = entry(
            "d",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            "{}",
        );
        json_escaped.post_data = Some(r#"{"password":"hunter\u0020two"}"#.to_string());
        assert!(
            carries_secret(&json_escaped, &secret),
            "json unicode escape"
        );
        let mut clean = entry(
            "e",
            "https://api.example.com.ar/login",
            "Fetch",
            "application/json",
            "{}",
        );
        clean.post_data = Some("user=a&note=hunter+three".to_string());
        assert!(!carries_secret(&clean, &secret));
    }

    #[test]
    fn a_write_takes_a_value_from_an_earlier_response_and_depends_on_it() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_now("session");
        r.begin_command(
            "click",
            &json!({ "action": "click", "selector": "#add" }),
            None,
            None,
        );
        r.tag_now("cart");
        let session = entry(
            "session",
            "https://api.example.com.ar/api/session",
            "Fetch",
            "application/json",
            r#"{"csrf":"a8f3c9d2e1b04f7a"}"#,
        );
        let mut cart = entry(
            "cart",
            "https://api.example.com.ar/api/cart",
            "Fetch",
            "application/json",
            r#"{"cart":[7]}"#,
        );
        cart.method = "POST".to_string();
        cart.post_data = Some(r#"{"id":7,"csrf":"a8f3c9d2e1b04f7a"}"#.to_string());
        let (flow, report) = build_flow_value(SaveInput {
            name: "c",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[session, cart],
            params: &[],
            output: Some("s2r1"),
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let body = flow["steps"][1]["requests"][0]["body"].as_str().unwrap();
        assert!(
            !body.contains("a8f3c9d2e1b04f7a"),
            "token saved literally: {body}"
        );
        assert!(body.contains("{{@s1r1.csrf}}"), "{body}");
        assert_eq!(
            dependencies(&flow_of(flow.clone()), "s2r1"),
            vec!["s1r1", "s2r1"]
        );
        assert!(report["references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["request"] == "s2r1"));
    }

    fn replayed(url: &str) -> HarEntry {
        let mut e = entry("r", url, "Fetch", "application/json", r#"{"items":[]}"#);
        e.status = Some(200);
        e
    }

    #[test]
    fn replay_match_keeps_pages_apart_and_ignores_cache_busters() {
        let entries = vec![
            replayed("https://api.site.example/items?page=1&_=111"),
            replayed("https://api.site.example/items?page=2&_=222"),
        ];
        let want = |url: &str| PreparedRequest {
            url: url.to_string(),
            ..prepared("s1r1")
        };
        let one = replay_lookup(&entries, &want("https://api.site.example/items?page=1"))
            .ok()
            .unwrap();
        assert!(one.url.contains("page=1"));
        let two = replay_lookup(&entries, &want("https://api.site.example/items?page=2"))
            .ok()
            .unwrap();
        assert!(two.url.contains("page=2"));
        // A recorded value that no replayed request has finds nothing.
        let err = replay_lookup(&entries, &want("https://api.site.example/items?page=3"))
            .err()
            .unwrap();
        assert!(err.contains("no replayed request"), "{err}");
        // A request without the query matches both pages, so it fails closed.
        let err = replay_lookup(&entries, &want("https://api.site.example/items"))
            .err()
            .unwrap();
        assert!(err.contains("several"), "{err}");
        // The same page twice, differing only by a cache-buster, is ambiguous too.
        let twice = vec![
            replayed("https://api.site.example/items?page=1&_=1"),
            replayed("https://api.site.example/items?page=1&_=2"),
        ];
        assert!(replay_lookup(&twice, &want("https://api.site.example/items?page=1")).is_err());
        // An exact URL still wins.
        assert!(replay_lookup(&twice, &want("https://api.site.example/items?page=1&_=2")).is_ok());
        assert!(
            replay_lookup(&entries, &want("https://api.site.example/items?page=1"))
                .ok()
                .is_some()
        );
        assert!(
            replay_lookup(&entries, &want("https://api.site.example/items"))
                .ok()
                .is_none()
        );
    }

    #[test]
    fn a_saved_flow_reads_and_rewrites_byte_for_byte() {
        let dir = std::env::temp_dir().join(format!("ab-flow-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.to_str().unwrap());
        let rec = recorder();
        let entries = vec![entry(
            "api",
            "https://api.example.com.ar/movies?film=Film+One",
            "Fetch",
            "application/json",
            r#"{"movies":[{"id":1}]}"#,
        )];
        let (flow, _) = build_flow(SaveInput {
            name: "rt",
            recorder: &rec,
            commands: &rec.commands,
            description: Some("Movies"),
            entries: &entries,
            params: &[("film".to_string(), "Film One".to_string())],
            output: None,
            created_at: "2026-11-03T00:00:00Z".to_string(),
            login_wall: false,
        })
        .unwrap();
        let path = write_flow(&flow).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        let loaded = load_flow("rt", None).unwrap();
        write_flow(&loaded).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_ids(flow: &Flow) -> Vec<String> {
        flow.run_writes().iter().map(|w| w.id.clone()).collect()
    }

    fn flow_with_request(tier: Option<&str>, write: bool) -> Flow {
        flow_of(json!({
            "output": "s1r1",
            "steps": [{ "command": {}, "requests": [{
                "id": "s1r1", "method": if write { "POST" } else { "GET" },
                "url": "https://a.test/x", "tier": tier
            }] }]
        }))
    }

    fn result(
        tier: Tier,
        ok: bool,
        status: Option<u16>,
        drift: bool,
        kind: Option<Stop>,
    ) -> TierResult {
        TierResult {
            id: "s1r1".to_string(),
            step: 1,
            ok,
            status,
            reason: None,
            body: None,
            ms: 0,
            drift,
            shape_changed: false,
            empty: false,
            tier,
            stop: kind,
            soft_stop: None,
            transport: false,
            retry_after: None,
        }
    }

    #[test]
    fn next_tier_follows_the_order_and_never_resends_a_write() {
        use Tier::*;
        // (preferred tier, write, attempts so far, expected next)
        type Case = (Option<&'static str>, bool, Vec<TierResult>, Option<Tier>);
        let cases: Vec<Case> = vec![
            (None, false, vec![], Some(Http)),
            (Some("page"), false, vec![], Some(Page)),
            (
                None,
                false,
                vec![result(Http, true, Some(200), false, None)],
                None,
            ),
            (
                None,
                false,
                vec![result(Http, false, Some(200), true, None)],
                Some(Page),
            ),
            (
                None,
                false,
                vec![result(Http, false, Some(400), false, None)],
                None,
            ),
            (
                None,
                false,
                vec![result(
                    Http,
                    false,
                    Some(403),
                    false,
                    Some(Stop::AuthExpired),
                )],
                None,
            ),
            (
                None,
                false,
                vec![
                    result(Http, false, Some(200), true, None),
                    result(Page, false, None, true, None),
                ],
                Some(Replay),
            ),
            (
                None,
                false,
                vec![result(Replay, false, None, true, None)],
                None,
            ),
            // A write the site rejected may go out again; an accepted one may not.
            (
                None,
                true,
                vec![result(Http, false, Some(500), false, None)],
                Some(Page),
            ),
            (
                None,
                true,
                vec![result(Http, false, Some(401), false, None)],
                Some(Page),
            ),
            (
                None,
                true,
                vec![result(Http, false, Some(303), false, None)],
                None,
            ),
            (
                None,
                true,
                vec![result(Http, false, None, false, Some(Stop::OutcomeUnknown))],
                None,
            ),
        ];
        for (i, (tier, write, attempts, want)) in cases.into_iter().enumerate() {
            let flow = flow_with_request(tier, write);
            let req = PreparedRequest {
                write,
                ..prepared("s1r1")
            };
            let got = next_tier(&flow, &req, &attempts, &WriteLedger::default());
            assert_eq!(got, want, "case {i}");
        }
    }

    #[test]
    fn a_sent_write_is_only_read_back_from_the_replay() {
        let flow = flow_with_request(None, true);
        let req = PreparedRequest {
            write: true,
            ..prepared("s1r1")
        };
        let mut ledger = WriteLedger::default();
        ledger.record(&req, &result(Tier::Http, false, Some(500), false, None));
        assert!(
            !ledger.already_sent("s1r1"),
            "a 5xx is not an accepted write"
        );
        ledger.record(&req, &result(Tier::Http, false, Some(401), false, None));
        assert!(
            !ledger.already_sent("s1r1"),
            "a rejection is not a sent write"
        );
        ledger.record(
            &req,
            &result(Tier::Http, false, None, false, Some(Stop::OutcomeUnknown)),
        );
        assert!(ledger.already_sent("s1r1"));
        assert_eq!(next_tier(&flow, &req, &[], &ledger), None);
        let mut flow_writes = flow.clone();
        flow_writes.writes = vec![Write {
            id: "s1r1".to_string(),
            method: "POST".to_string(),
            url: String::new(),
            kind: WriteKind::Explicit,
            step: 1,
        }];
        let mut after_replay = WriteLedger::default();
        after_replay.replay_ran(&flow_writes);
        assert_eq!(
            next_tier(&flow, &req, &[], &after_replay),
            Some(Tier::Replay)
        );
        assert_eq!(
            after_replay
                .first_repeated_by_replay(&flow_writes)
                .as_deref(),
            Some("s1r1")
        );
    }

    fn run_flow(rows: Value) -> (Flow, Vec<PreparedRequest>) {
        let flow = flow_of(json!({
            "output": rows["output"],
            "writes": rows["writes"],
            "steps": rows["steps"],
        }));
        let requests = prepare(&flow, &[]).unwrap();
        (flow, requests)
    }

    fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    const OK_JSON: &str = r#"{"ok":true}"#;

    fn ok_expect() -> Value {
        json!({ "signature": signature(&json!({ "ok": true })) })
    }

    #[test]
    fn the_run_loop_sends_an_accepted_write_once_and_falls_back_for_reads() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s2r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [
                { "command": {}, "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "expect": ok_expect() }] },
                { "command": {}, "requests": [{ "id": "s2r1", "method": "GET", "url": "https://a.test/items?c={{@s1r1.ok}}", "expect": ok_expect() }] }
            ]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::json(200, OK_JSON))
            .answer(
                Tier::Http,
                "s2r1",
                Scripted::json(200, "<html>sign in</html>"),
            )
            .answer(Tier::Page, "s2r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert!(results.iter().all(|r| r.ok || r.tier == Tier::Http));
        assert_eq!(senders.times("s1r1"), 1);
        assert_eq!(
            senders.sent,
            vec![
                (Tier::Http, "s1r1".to_string()),
                (Tier::Http, "s2r1".to_string()),
                (Tier::Page, "s2r1".to_string()),
            ]
        );
    }

    #[test]
    fn the_run_loop_never_resends_a_write_the_replay_sent() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s2r1",
            "writes": [{ "id": "s2r1", "method": "POST", "kind": "explicit" }],
            "steps": [
                { "command": {}, "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/session", "tier": "replay", "expect": ok_expect() }] },
                { "command": {}, "requests": [{ "id": "s2r1", "method": "POST", "url": "https://a.test/cart?c={{@s1r1.ok}}", "expect": ok_expect() }] }
            ]
        }));
        // The replay answers the session read and, having repeated every
        // command, the write too; the write is then read back, not sent.
        let mut senders = MemorySenders::default()
            .answer(Tier::Replay, "s1r1", Scripted::json(200, OK_JSON))
            .answer(Tier::Replay, "s2r1", Scripted::json(200, OK_JSON))
            .answer(Tier::Http, "s2r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert!(results.iter().all(|r| r.ok));
        assert_eq!(
            senders.sent,
            vec![
                (Tier::Replay, "s1r1".to_string()),
                (Tier::Replay, "s2r1".to_string()),
            ],
            "the write is answered by the replay, not by HTTP"
        );
    }

    #[test]
    fn the_run_loop_stops_on_an_unknown_write_outcome() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::unknown())
            .answer(Tier::Page, "s1r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert_eq!(results.last().unwrap().stop, Some(Stop::OutcomeUnknown));
        assert_eq!(senders.times("s1r1"), 1);
    }

    #[test]
    fn a_write_answered_with_a_server_error_is_not_sent_again() {
        use memory::{MemorySenders, Scripted};
        for status in [500u16, 502, 503, 408] {
            let (flow, requests) = run_flow(json!({
                "output": "s1r1",
                "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
                "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "expect": ok_expect() }] }]
            }));
            let mut senders = MemorySenders::default()
                .answer(Tier::Http, "s1r1", Scripted::json(status, "{}"))
                .answer(Tier::Page, "s1r1", Scripted::json(200, OK_JSON));
            let results =
                block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
            assert_eq!(
                senders.sent,
                vec![(Tier::Http, "s1r1".to_string())],
                "{status}"
            );
            assert_eq!(results.last().unwrap().stop, Some(Stop::OutcomeUnknown));
        }
        // A 4xx rejection means the write was not applied; the page may still send it.
        let (flow, requests) = run_flow(json!({
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::json(422, "{}"))
            .answer(Tier::Page, "s1r1", Scripted::json(200, OK_JSON));
        block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert_eq!(senders.times("s1r1"), 2);
    }

    #[test]
    fn the_run_loop_refuses_a_request_the_check_rejects() {
        use memory::MemorySenders;
        let (flow, requests) = run_flow(json!({
            "output": "s1r1", "writes": [],
            "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "GET", "url": "https://evil.test/x", "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default();
        let check = |r: &PreparedRequest| Err(format!("{} refused", r.url));
        let err = block_on(run_requests(&flow, &requests, &mut senders, &check))
            .err()
            .unwrap();
        assert!(err.contains("evil.test"));
        assert!(senders.sent.is_empty());
    }

    #[test]
    fn the_page_tier_needs_a_tab_on_the_flows_site() {
        let flow = flow_of(json!({ "startUrl": "https://example.com/" }));
        for (tab, ok) in [
            (None, false),
            (Some("https://evil.test/"), false),
            (Some("https://app.example.com/x"), true),
        ] {
            assert_eq!(tab_on_site(&flow, tab).is_ok(), ok, "{tab:?}");
        }
        let err = tab_on_site(&flow, None).unwrap_err();
        assert!(
            err.contains("example.com") && err.contains("open the flow's site"),
            "{err}"
        );
    }

    #[test]
    fn host_policy_allows_the_start_domain_and_nothing_else() {
        // (start URL, allowedHosts in the file, URL, allowed)
        let cases: &[(&str, &[&str], &str, bool)] = &[
            (
                "https://www.example.com/shop",
                &["example.com"],
                "https://api.example.com/x",
                true,
            ),
            (
                "https://www.example.com/shop",
                &["example.com"],
                "https://example.com/x",
                true,
            ),
            (
                "https://www.example.com/",
                &["example.com"],
                "https://evil.com/x",
                false,
            ),
            (
                "https://www.example.com/",
                &["example.com"],
                "https://example.com.evil.com/x",
                false,
            ),
            (
                "https://www.example.com/",
                &["example.com"],
                "https://notexample.com/x",
                false,
            ),
            (
                "https://www.example.com/",
                &["example.com"],
                "ftp://example.com/x",
                false,
            ),
            (
                "https://www.example.com/",
                &["example.com"],
                "not a url",
                false,
            ),
            // The file's list can narrow the start domain but never widen it.
            (
                "https://example.com/",
                &["example.com", "evil.com"],
                "https://evil.com/x",
                false,
            ),
            (
                "https://example.com/",
                &["api.example.com"],
                "https://www.example.com/x",
                false,
            ),
            (
                "https://example.com/",
                &["api.example.com"],
                "https://api.example.com/x",
                true,
            ),
            // IP hosts match exactly.
            (
                "http://127.0.0.1:3000/",
                &["127.0.0.1"],
                "http://127.0.0.1:3000/api",
                true,
            ),
            (
                "http://127.0.0.1:3000/",
                &["127.0.0.1"],
                "http://10.0.0.1/api",
                false,
            ),
            // The public suffix list keeps sibling sites apart.
            (
                "https://a.vercel.app/",
                &["a.vercel.app"],
                "https://api.a.vercel.app/x",
                true,
            ),
            (
                "https://a.vercel.app/",
                &["a.vercel.app"],
                "https://b.vercel.app/x",
                false,
            ),
            (
                "https://shop.example.com.ar/",
                &[],
                "https://api.example.com.ar/x",
                true,
            ),
        ];
        for (start, listed, url, allowed) in cases {
            let flow = flow_of(json!({ "startUrl": start, "allowedHosts": listed }));
            assert_eq!(
                HostPolicy::of(&flow).check(url).is_ok(),
                *allowed,
                "{start} -> {url}"
            );
        }
        assert_eq!(base_domain("127.0.0.1"), "127.0.0.1");
        assert_ne!(base_domain("a.vercel.app"), base_domain("b.vercel.app"));
        assert_eq!(base_domain("localhost"), "localhost");
        assert_eq!(base_domain("api.example.com.ar"), "example.com.ar");
    }

    #[test]
    fn a_denied_request_that_needed_a_stripped_credential_is_not_an_expired_session() {
        let mut req = prepared("s1r1");
        for status in [401, 403] {
            let f = check_response(&req, status, "{}").unwrap_err();
            assert_eq!(f.stop, Some(Stop::AuthExpired), "{status}");
        }
        req.expect.stripped_auth = true;
        for status in [401, 403] {
            let f = check_response(&req, status, "{}").unwrap_err();
            assert_eq!(f.stop, Some(Stop::CredentialNotSendable), "{status}");
        }
        assert_eq!(
            Stop::CredentialNotSendable.code(),
            "credential_not_sendable"
        );
        // Other statuses are judged the same either way.
        let f = check_response(&req, 429, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::RateLimited));
    }

    #[test]
    fn a_denied_request_with_a_recorded_signature_is_not_an_expired_session() {
        let mut req = prepared("s1r1");
        req.headers
            .push(("X-Ts".to_string(), "1730000000".to_string()));
        req.headers.push((
            "X-Sig".to_string(),
            "9f2c41d8a7b3e6f05c1d2a9b8e7f6a43".to_string(),
        ));
        for status in [401, 403] {
            let f = check_response(&req, status, "{}").unwrap_err();
            assert_eq!(f.stop, Some(Stop::RequestSigned), "{status}");
            assert!(
                f.reason.contains("X-Ts") && f.reason.contains("X-Sig"),
                "{}",
                f.reason
            );
        }
        assert_eq!(Stop::RequestSigned.code(), "request_signed");
        // A credential the file cannot hold is the more basic problem.
        req.expect.stripped_auth = true;
        let f = check_response(&req, 401, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::CredentialNotSendable));
        // Ordinary headers, including ones that only contain these letters, are not signatures.
        let mut plain = prepared("s1r1");
        plain
            .headers
            .push(("Accepts-Language".to_string(), "en".to_string()));
        plain
            .headers
            .push(("X-Requested-With".to_string(), "fetch".to_string()));
        let f = check_response(&plain, 401, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::AuthExpired));
        // A signature header does not change how other statuses are judged.
        assert!(check_response(&req, 200, r#"{"data":[{"id":1}]}"#).is_ok());
    }

    #[test]
    fn a_header_named_like_a_signature_needs_a_value_that_looks_per_request() {
        let sig = "9f2c41d8a7b3e6f05c1d2a9b8e7f6a43";
        let names = |pairs: &[(&str, &str)]| {
            let headers: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            signature_headers(&headers)
        };
        // Per-request values.
        for (name, value) in [
            ("X-Ts", "1730000000"),
            ("x_timestamp", "1730000000123"),
            ("XSig", sig),
            ("HMACSignature", sig),
            ("X-Request-Nonce", sig),
            ("x-hmac", "aGVsbG8gd29ybGQgdGhpcyBpcyBhIHNpZw=="),
            ("X-Ts", "1730000000.123"),
            ("X-Timestamp", "2026-11-04T10:15:30Z"),
            ("X-Timestamp", "2026-11-04T10:15:30.123+00:00"),
            ("X-Signature", "qZkWvbRtYhGnMcLpXdSfUaJe7"),
            ("X-Nonce", "123e4567-e89b-12d3-a456-426614174000"),
            ("X-Signature", "mQ2vN8xLr4TzKcWb9YdHf3JsPa6UeGo1VtXi5BnRq7A"),
            (
                "X-Sig",
                "9f2c41d8a7b3e6f05c1d2a9b8e7f6a439f2c41d8a7b3e6f05c1d2a9b8e7f6a43",
            ),
            (
                "Stripe-Signature",
                "t=1730000000,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e77a0e56ff536d0ce8e108d8bd",
            ),
        ] {
            assert_eq!(names(&[(name, value)]), vec![name.to_string()], "{name}");
        }
        // Names that share a word with a signature, or a static value.
        for (name, value) in [
            ("x-ts", "web"),
            ("X-Signed-In", "true"),
            ("x-user-signed-in", "1"),
            ("X-Timestamp-Format", "iso"),
            ("X-TS-Env", "prod"),
            ("x-sig-version", "2"),
            ("X-Client-Nonce-Mode", "strict"),
            ("Accept-Signature", "sig1"),
            ("Accepts-Language", "en"),
            ("X-Timestamp", "iso"),
            ("X-Timestamp", "1730000000x"),
            ("X-Signature", "signatureversion"),
            ("X-Signature", "SignatureVersion"),
            ("X-Signature-Version", "SignatureVersion2"),
            ("X-Sig-Alg", "RSASSA-PSS-SHA256"),
            ("X-Signature-Alg", "ECDSA-P256-SHA256"),
            ("X-Signature-Type", "AwsSignatureV4Hmac"),
            ("X-Signature-Key-Id", "AppKeyProduction01"),
            ("X-Nonce-Source", "BrowserCryptoApi"),
            ("X-Signature", "qZkWvbRtYhGnMcLpXdSfUaJe"),
            ("X-Signature", "t=1,v=2"),
            ("X-Signature", "version=2,alg=sha256"),
        ] {
            assert!(names(&[(name, value)]).is_empty(), "{name}");
        }
    }

    #[test]
    fn a_header_value_repeated_across_requests_is_static() {
        let flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r2",
            "steps": [{ "requests": [
                { "id": "s1r1", "method": "GET", "url": "https://a.test/x",
                  "headers": { "X-Sig": "9f2c41d8a7b3e6f05c1d2a9b8e7f6a43", "X-Ts": "1730000000" } },
                { "id": "s1r2", "method": "GET", "url": "https://a.test/y",
                  "headers": { "X-Sig": "9f2c41d8a7b3e6f05c1d2a9b8e7f6a43", "X-Ts": "1730000099" } },
            ] }],
        }));
        let reqs = prepare_all(&flow, &[]).unwrap();
        let f = check_response(&reqs[1], 401, "{}").unwrap_err();
        // X-Sig repeats, so it is static; X-Ts differs, so it is per request.
        assert_eq!(f.stop, Some(Stop::RequestSigned));
        assert!(
            f.reason.contains("X-Ts") && !f.reason.contains("X-Sig"),
            "{}",
            f.reason
        );
        let mut only_sig = reqs[1].clone();
        only_sig.headers.retain(|(k, _)| k == "X-Sig");
        let f = check_response(&only_sig, 401, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::AuthExpired));
        assert!(f.reason.contains("X-Sig"), "{}", f.reason);
    }

    #[test]
    fn a_static_timestamp_style_header_does_not_hide_an_expired_session() {
        let mut req = prepared("s1r1");
        req.headers.push(("x-ts".to_string(), "web".to_string()));
        let f = check_response(&req, 401, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::AuthExpired));
        assert!(f.reason.contains("x-ts"), "{}", f.reason);
        let mut plain = prepared("s1r1");
        plain
            .headers
            .push(("X-Signed-In".to_string(), "true".to_string()));
        let f = check_response(&plain, 401, "{}").unwrap_err();
        assert_eq!(f.stop, Some(Stop::AuthExpired));
    }

    #[test]
    fn check_response_judges_status_body_and_shape() {
        // (status, body, expected: None = data, Some((kind, drift, shape_changed)))
        type Case = (u16, &'static str, Option<(Option<Stop>, bool, bool)>);
        let cases: Vec<Case> = vec![
            (200, r#"{"data":[{"id":2}]}"#, None),
            (200, r#"{"data":[{"id":1,"note":"captcha solved"}]}"#, None),
            (200, r#"{"items":[]}"#, Some((None, true, true))),
            (200, "<html>sign in</html>", Some((None, true, false))),
            (
                200,
                "<html><body>captcha</body></html>",
                Some((Some(Stop::Blocked), false, false)),
            ),
            (
                403,
                "<div id=cf-chl>",
                Some((Some(Stop::Blocked), false, false)),
            ),
            (401, "{}", Some((Some(Stop::AuthExpired), false, false))),
            (403, "", Some((Some(Stop::AuthExpired), false, false))),
            (429, "{}", Some((Some(Stop::RateLimited), false, false))),
            (400, "{}", Some((None, false, false))),
            (500, "{}", Some((None, false, false))),
            (407, "{}", Some((None, true, false))),
        ];
        let req = prepared("s1r1");
        for (status, body, want) in cases {
            match (check_response(&req, status, body), want) {
                (Ok(_), None) => {}
                (Err(f), Some((kind, drift, shape))) => {
                    assert_eq!(
                        (f.stop, f.drift, f.shape_changed),
                        (kind, drift, shape),
                        "{status} {body}"
                    );
                }
                (got, want) => panic!("{status} {body}: got ok={} want {want:?}", got.is_ok()),
            }
        }
    }

    #[test]
    fn graphql_queries_are_reads_and_mutations_are_writes() {
        // (method, body, write)
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("GET", None, false),
            ("POST", Some(r#"{"a":1}"#), true),
            ("POST", Some(r#"{"query":"query appConfig { x }"}"#), false),
            ("POST", Some(r#"[{"query":"{ me { id } }"}]"#), false),
            ("POST", Some(r#"{"query":"mutation buy { order }"}"#), true),
            ("DELETE", None, true),
        ];
        for (method, body, write) in cases {
            assert_eq!(is_write(method, *body), *write, "{method} {body:?}");
        }
    }

    #[test]
    fn a_field_is_secret_by_type_autocomplete_or_name() {
        // (type, autocomplete, hint, secret)
        let cases: &[(&str, &str, &str, bool)] = &[
            ("password", "", "", true),
            ("text", "one-time-code", "", true),
            ("text", "current-password", "", true),
            ("text", "new-password", "", true),
            ("text", "cc-number", "", true),
            ("tel", "section-pay cc-csc", "", true),
            ("text", "", "Your PIN", true),
            ("text", "", "otp", true),
            ("text", "email", "email", false),
            ("text", "", "search", false),
        ];
        for (kind, autocomplete, hint, secret) in cases {
            let t = traits(kind, autocomplete, hint);
            assert_eq!(t.looks_secret(), *secret, "{t:?}");
            let mut r = recorded(&[]);
            r.begin_command(
                "fill",
                &json!({ "action": "fill", "selector": "@e5", "value": "482913" }),
                None,
                Some(&t),
            );
            assert_eq!(r.commands[0].1["value"] == REDACTED, *secret, "{t:?}");
            if *secret {
                assert_eq!(r.commands[0].1["redacted"], true);
                assert_withheld(&mut r, "482913");
            }
        }
    }

    fn with_timeout<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("did not finish: the expansion loops")
    }

    #[test]
    fn a_response_value_with_its_own_reference_expands_once_and_ends() {
        let out = with_timeout(|| {
            let bodies = HashMap::from([("a".to_string(), json!({ "v": "{{@a.v}}", "w": "ok" }))]);
            let req = PreparedRequest {
                method: "PUT".to_string(),
                write: true,
                body: Some("x={{@a.v}}&y={{@a.w}}".to_string()),
                ..prepared("s2r1")
            };
            let one = req.resolve(&bodies).unwrap().body.unwrap();
            let req = PreparedRequest {
                body: Some("{{@a.w}}{{@a.v}}".to_string()),
                ..req
            };
            (one, req.resolve(&bodies).unwrap().body.unwrap())
        });
        assert_eq!(out.0, "x=%7B%7B%40a.v%7D%7D&y=ok");
        assert_eq!(out.1, "ok{{@a.v}}");
    }

    #[test]
    fn a_param_value_with_placeholder_text_is_inserted_literally() {
        let flow = flow_of(json!({
            "params": [
                { "name": "a", "example": "{{b}}" },
                { "name": "b", "example": "X" },
                { "name": "c", "example": "{{@s1r1.id}}" }
            ],
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "PUT", "kind": "explicit" }],
            "steps": [{ "requests": [{ "id": "s1r1", "method": "PUT", "url": "https://a.test/x",
                "body": "{{a}}-{{b}}-{{c}}" }] }]
        }));
        let sent = prepare_all(&flow, &[]).unwrap();
        assert_eq!(sent[0].body.as_deref(), Some("{{b}}-X-{{@s1r1.id}}"));
        // A value that looks like a reference is data, not a reference.
        let bodies = HashMap::from([("s1r1".to_string(), json!({ "id": "7" }))]);
        let resolved = sent[0].resolve(&bodies).unwrap();
        assert_eq!(resolved.body.as_deref(), Some("{{b}}-X-{{@s1r1.id}}"));
        let missing = flow_of(json!({
            "output": "s1r1",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/{{nope}}" }] }]
        }));
        assert!(prepare_all(&missing, &[])
            .err()
            .unwrap()
            .contains("missing --param nope"));
    }

    #[test]
    fn a_graphql_operation_with_a_placeholder_is_a_write() {
        for body in [
            r#"{"query":"{{op}} X { a }"}"#,
            r#"[{"query":"query A { a }"},{"query":"query B { {{@s1r1.f}} }"}]"#,
        ] {
            assert!(is_write("POST", Some(body)), "{body}");
        }
        assert!(!is_write(
            "POST",
            Some(r#"{"query":"query X { a }","variables":{"q":"{{q}}"}}"#)
        ));
    }

    #[test]
    fn a_param_cannot_turn_a_saved_query_into_a_mutation() {
        let flow = flow_of(json!({
            "params": [{ "name": "op", "example": "query" }],
            "output": "s1r1", "writes": [],
            "steps": [{ "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/graphql",
                "body": "{\"query\":\"{{op}} X { a }\"}" }] }]
        }));
        let err = prepare_all(
            &flow,
            &[("op".to_string(), "mutation Y { buy }".to_string())],
        )
        .err()
        .unwrap();
        assert!(err.contains("s1r1") && err.contains("write"), "{err}");
    }

    #[test]
    fn a_param_cannot_put_a_mutating_segment_in_a_read_url() {
        let flow = flow_of(json!({
            "params": [{ "name": "id", "example": "42" }],
            "output": "s1r1", "writes": [],
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/items/{{id}}" }] }]
        }));
        assert!(prepare_all(&flow, &[]).is_ok());
        let err = prepare_all(&flow, &[("id".to_string(), "delete".to_string())])
            .err()
            .unwrap();
        assert!(err.contains("s1r1") && err.contains("write"), "{err}");
        let req = PreparedRequest {
            url: "https://a.test/items/{{@a.id}}".to_string(),
            write: false,
            ..prepared("s2r1")
        };
        let bodies = HashMap::from([("a".to_string(), json!({ "id": "remove" }))]);
        assert!(req.resolve(&bodies).err().unwrap().contains("write"));
    }

    #[test]
    fn a_flow_without_an_output_gates_every_write_it_sends() {
        for output in [json!(null), json!("")] {
            let mut v = json!({
                "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
                "steps": [{ "requests": [
                    { "id": "s1r1", "method": "POST", "url": "https://a.test/cart" },
                    { "id": "s1r2", "method": "GET", "url": "https://a.test/items" }
                ] }]
            });
            if !output.is_null() {
                v["output"] = output;
            }
            let flow = flow_of(v);
            let sent: Vec<String> = prepare(&flow, &[])
                .unwrap()
                .into_iter()
                .map(|r| r.id)
                .collect();
            assert_eq!(sent, vec!["s1r1", "s1r2"]);
            assert_eq!(write_ids(&flow), vec!["s1r1"]);
        }
    }

    #[test]
    fn an_empty_dependency_does_not_empty_a_nonempty_output() {
        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s2r1",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        let dependency = TierResult {
            id: "s1r1".to_string(),
            empty: true,
            body: Some(json!({ "payload": {} })),
            ..result(Tier::Http, true, Some(200), false, None)
        };
        let output = TierResult {
            id: "s2r1".to_string(),
            step: 2,
            body: Some(json!({ "items": [{ "id": 7 }] })),
            ..result(Tier::Http, true, Some(200), false, None)
        };
        let out = finish_run(
            &mut flow,
            &[dependency, output],
            "now",
            1,
            &Shape::default(),
        );
        assert_eq!(out["data"]["items"][0]["id"], 7, "{out}");
        assert!(out.get("empty").is_none(), "{out}");
    }

    #[test]
    fn a_form_param_is_encoded_into_its_field() {
        let flow = flow_of(json!({
            "params": [{ "name": "q", "example": "a" }],
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/s",
                "headers": { "content-type": "application/x-www-form-urlencoded" },
                "body": "a={{q}}&b=1" }] }]
        }));
        let sent = prepare_all(&flow, &[("q".to_string(), "x&b=2+3 4".to_string())]).unwrap();
        assert_eq!(sent[0].body.as_deref(), Some("a=x%26b%3D2%2B3%204&b=1"));
    }

    fn save_one_post(body: String, form: bool, params: &[(String, String)]) -> Value {
        let mut r = recorder();
        r.tag_now("p");
        let mut e = entry(
            "p",
            "https://api.example.com.ar/s",
            "Fetch",
            "application/json",
            r#"{"items":[{"id":1}]}"#,
        );
        e.method = "POST".to_string();
        if form {
            e.request_headers.push((
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ));
        }
        e.post_data = Some(body);
        let (flow, _) = build_flow_value(SaveInput {
            name: "p",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[e],
            params,
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        flow["steps"][1]["requests"][0].clone()
    }

    #[test]
    fn a_typed_value_with_quotes_backslashes_and_newlines_is_found_in_a_json_body() {
        let value = "a \"b\" \\ c\nd".to_string();
        let body = serde_json::to_string(&json!({ "q": value, "n": 1 })).unwrap();
        let saved = save_one_post(body, false, &[("q".to_string(), value.clone())]);
        let text = saved["body"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["q"], "{{q}}", "{text}");
        assert!(!text.contains("c\\nd"), "{text}");
    }

    #[test]
    fn a_typed_value_is_found_in_its_encoded_form_in_a_form_body() {
        let params = [("q".to_string(), "x&y+1 z".to_string())];
        for body in ["a=x%26y%2B1%20z&b=2", "a=x%26y%2B1+z&b=2"] {
            let saved = save_one_post(body.to_string(), true, &params);
            assert_eq!(saved["body"], "a={{q}}&b=2", "{body}");
        }
    }

    fn post_flow(headers: Value, body: &str) -> Flow {
        flow_of(json!({
            "params": [{ "name": "q", "example": "x" }],
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/s",
                "headers": headers, "body": body }] }]
        }))
    }

    #[test]
    fn a_form_shaped_body_without_a_form_content_type_keeps_one_field_per_param() {
        for headers in [
            json!({ "content-type": "text/plain;charset=UTF-8" }),
            json!({}),
        ] {
            let flow = post_flow(headers, "a={{q}}&b=1");
            let sent = prepare_all(&flow, &[("q".to_string(), "a&b=c".to_string())]).unwrap();
            assert_eq!(sent[0].body.as_deref(), Some("a=a%26b%3Dc&b=1"));
        }
    }

    #[test]
    fn a_value_that_could_break_a_multipart_or_raw_body_is_refused() {
        let multipart =
            "--XyZ\r\nContent-Disposition: form-data; name=\"f\"\r\n\r\n{{q}}\r\n--XyZ--\r\n";
        let flow = post_flow(
            json!({ "content-type": "multipart/form-data; boundary=XyZ" }),
            multipart,
        );
        let refused = |flow: &Flow, value: &str| {
            prepare_all(flow, &[("q".to_string(), value.to_string())])
                .err()
                .unwrap_or_default()
        };
        assert!(refused(&flow, "v\r\n--XyZ\r\nContent-Disposition: x").contains("line break"));
        assert!(refused(&flow, "has XyZ inside").contains("boundary"));
        assert!(prepare_all(&flow, &[("q".to_string(), "plain".to_string())]).is_ok());
        // Without the header the shape still says multipart.
        let bare = post_flow(json!({}), multipart);
        assert!(refused(&bare, "a\nb").contains("line break"));
        // Plain text and header values take no line break either.
        let text = post_flow(json!({ "content-type": "text/plain" }), "hello {{q}}");
        assert!(refused(&text, "a\nb").contains("line break"));
        assert!(prepare_all(&text, &[("q".to_string(), "a b".to_string())]).is_ok());
        let header = flow_of(json!({
            "params": [{ "name": "q", "example": "x" }], "output": "s1r1",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/s",
                "headers": { "x-q": "{{q}}" } }] }]
        }));
        // A header with a refused value is dropped, never sent with a line break.
        let sent = prepare_all(&header, &[("q".to_string(), "a\r\nX: 1".to_string())]).unwrap();
        assert!(sent[0].headers.is_empty());
    }

    #[test]
    fn a_reference_cannot_turn_a_read_into_a_write() {
        let req = PreparedRequest {
            body: Some(r#"{"query":"{{@a.op}} X { a }"}"#.to_string()),
            method: "POST".to_string(),
            write: false,
            ..prepared("s2r1")
        };
        let bodies = HashMap::from([("a".to_string(), json!({ "op": "mutation Y { buy }" }))]);
        assert!(req.resolve(&bodies).err().unwrap().contains("write"));
        let ok = HashMap::from([("a".to_string(), json!({ "op": "query" }))]);
        assert!(req.resolve(&ok).is_ok());
    }

    #[test]
    fn save_keeps_a_graphql_operation_literal_and_templatizes_variables() {
        let mut r = recorder();
        r.tag_now("gql");
        let mut e = entry(
            "gql",
            "https://api.example.com.ar/graphql",
            "Fetch",
            "application/json",
            r#"{"data":{"search":[{"id":1}]}}"#,
        );
        e.method = "POST".to_string();
        e.post_data = Some(
            r#"{"query":"query S($t:String){ search(term:\"kettle\", t:$t){ id } }","variables":{"t":"kettle"}}"#
                .to_string(),
        );
        let params = vec![("product".to_string(), "kettle".to_string())];
        let (flow, _) = build_flow_value(SaveInput {
            name: "g",
            recorder: &r,
            commands: &r.commands,
            description: None,
            entries: &[e],
            params: &params,
            output: None,
            created_at: String::new(),
            login_wall: false,
        })
        .unwrap();
        let body: Value =
            serde_json::from_str(flow["steps"][1]["requests"][0]["body"].as_str().unwrap())
                .unwrap();
        assert!(body["query"].as_str().unwrap().contains("kettle"));
        assert!(!body["query"].as_str().unwrap().contains("{{"));
        assert_eq!(body["variables"]["t"], "{{product}}");
        assert_eq!(flow["writes"], json!([]));
    }

    #[test]
    fn a_request_is_a_write_exactly_when_the_writes_list_says_so() {
        let flow = flow_of(json!({
            "output": "s1r2",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "command": {}, "requests": [
                { "id": "s1r1", "method": "POST", "url": "https://a.test/cart" },
                { "id": "s1r2", "method": "GET", "url": "https://a.test/items" }
            ] }]
        }));
        let prepared = prepare_all(&flow, &[]).unwrap();
        assert_eq!(
            prepared.iter().map(|r| r.write).collect::<Vec<_>>(),
            vec![true, false]
        );
        // The file has no second flag to disagree with the list.
        let mut with_flag = serde_json::to_value(&flow).unwrap();
        with_flag["steps"][0]["requests"][0]["write"] = json!(false);
        assert!(serde_json::from_value::<Flow>(with_flag).is_err());
    }

    fn attempt_result(
        tier: Tier,
        status: Option<u16>,
        reason: &str,
        drift: bool,
        stop: Option<Stop>,
        retry_after: Option<&str>,
    ) -> TierResult {
        TierResult {
            reason: Some(reason.to_string()),
            retry_after: retry_after.map(String::from),
            ..result(tier, false, status, drift, stop)
        }
    }

    #[test]
    fn a_run_outcome_names_what_stopped_the_run() {
        // (results, flow name, expected message, expected data status)
        let cases: Vec<(Vec<TierResult>, &str, Option<&str>)> = vec![
            (
                vec![attempt_result(Tier::Http, Some(429), "HTTP 429", false, Some(Stop::RateLimited), Some("17"))],
                "flow 'cart' step 1 (http): HTTP 429; retry after 17. Wait before rerunning",
                Some("rate_limited"),
            ),
            (
                vec![attempt_result(Tier::Http, Some(401), "HTTP 401", false, Some(Stop::AuthExpired), None)],
                "flow 'cart' step 1 (http): HTTP 401. Sign in again in the browser, then rerun",
                Some("auth_expired"),
            ),
            (
                vec![
                    attempt_result(Tier::Http, Some(401), "soft", true, None, None),
                    attempt_result(Tier::Page, Some(403), "soft", true, None, None),
                ],
                "flow 'cart' step 1 (page): soft. The flow cannot send the credential this request needs; use the page UI for this task",
                Some("credential_not_sendable"),
            ),
            (
                vec![attempt_result(Tier::Http, None, "sent, no answer", false, Some(Stop::OutcomeUnknown), None)],
                "flow 'cart' step 1 (http): sent, no answer. Check the site before rerunning; the write is not sent again",
                Some("outcome_unknown"),
            ),
        ];
        for (results, tail, status) in cases {
            let outcome = RunOutcome::stopped(&results).expect("a stop");
            let message = outcome.message("cart");
            assert!(message.ends_with(tail), "{message}");
            assert!(message.starts_with(status.unwrap()), "{message}");
            assert_eq!(outcome.data("cart").unwrap()["status"], status.unwrap());
        }
        let fine = vec![result(Tier::Http, true, Some(200), false, None)];
        assert_eq!(RunOutcome::stopped(&fine), None);
    }

    #[test]
    fn a_failed_run_says_drift_or_rejected_params() {
        let flow = flow_of(json!({ "params": [{ "name": "q", "example": "kettle" }] }));
        let drift = vec![
            attempt_result(
                Tier::Http,
                Some(200),
                "response is not JSON",
                true,
                None,
                None,
            ),
            attempt_result(Tier::Page, None, "skipped", true, None, None),
        ];
        let outcome = RunOutcome::failed(&drift, &flow);
        assert_eq!(
            outcome.message("f"),
            "flow 'f' failed at step 1 (http: response is not JSON; page: skipped). The flow is now marked unverified; drive the browser for this task instead"
        );
        assert_eq!(outcome.data("f"), None);
        let rejected = vec![attempt_result(
            Tier::Http,
            Some(400),
            "HTTP 400",
            false,
            None,
            None,
        )];
        assert_eq!(
            RunOutcome::failed(&rejected, &flow).message("f"),
            "flow 'f' failed at step 1: HTTP 400. The site likely rejected the param values; the recorded run used q=kettle"
        );
        let unreachable = vec![attempt_result(
            Tier::Http,
            None,
            "request failed",
            false,
            None,
            None,
        )];
        assert_eq!(
            RunOutcome::failed(&unreachable, &flow).message("f"),
            "flow 'f' failed at step 1: request failed"
        );
    }

    #[test]
    fn a_request_belongs_to_the_command_running_when_it_started() {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
        let click = json!({ "action": "click", "selector": "#a" });
        r.begin_command_at(100.0, "click", &click, None, None);
        r.begin_command_at(101.0, "click", &click, None, None);
        // Read after the second command began, but started during the first.
        r.tag("late", 100.5);
        r.tag("second", 101.5);
        r.tag("before", 99.0);
        assert_eq!(r.request_tags.get("late"), Some(&1));
        assert_eq!(r.request_tags.get("second"), Some(&2));
        assert_eq!(r.request_tags.get("before"), None);
        // A command that is not recorded owns the requests that start in it.
        r.untag_current();
        r.tag("unrecorded", f64::MAX);
        assert_eq!(r.request_tags.get("unrecorded"), None);
    }

    #[test]
    fn a_body_with_unfilled_references_matches_on_the_request_alone() {
        let mut sent = entry(
            "w",
            "https://api.site.example/cart",
            "Fetch",
            "application/json",
            "{}",
        );
        sent.method = "POST".to_string();
        sent.post_data = Some(r#"{"id":7,"csrf":"tok"}"#.to_string());
        let want = PreparedRequest {
            method: "POST".to_string(),
            url: "https://api.site.example/cart".to_string(),
            body: Some(r#"{"id":7,"csrf":"{{@s1r1.csrf}}"}"#.to_string()),
            ..prepared("s2r1")
        };
        assert!(replay_lookup(&[sent.clone()], &want).is_ok());
        let filled = PreparedRequest {
            body: Some(r#"{"csrf":"other","id":7}"#.to_string()),
            ..want
        };
        assert!(replay_lookup(&[sent], &filled).is_err());
    }

    fn signed_request(id: &str) -> PreparedRequest {
        let mut req = prepared(id);
        req.headers
            .push(("X-Ts".to_string(), "1730000000".to_string()));
        req
    }

    fn denied_attempt(req: &PreparedRequest, tier: Tier, status: u16) -> TierResult {
        tier_result(req, tier, Instant::now(), Ok((status, "{}".to_string())))
    }

    #[test]
    fn a_signed_request_denied_on_http_or_page_falls_to_the_next_tier() {
        let req = signed_request("s1r1");
        for tier in [Tier::Http, Tier::Page] {
            let r = denied_attempt(&req, tier, 401);
            assert_eq!(r.stop, None, "{tier:?}");
            assert!(r.drift, "{tier:?}");
        }
        let http = denied_attempt(&req, Tier::Http, 403);
        let page = denied_attempt(&req, Tier::Page, 403);
        // No replay available: the run reports the signature.
        let outcome = RunOutcome::stopped(&[http.clone(), page.clone()]).unwrap();
        assert!(
            matches!(
                outcome,
                RunOutcome::Stopped {
                    stop: Stop::RequestSigned,
                    ..
                }
            ),
            "{outcome:?}"
        );
        // The replay got a real 401 with the site's own signing: signed out.
        let replay = denied_attempt(&req, Tier::Replay, 401);
        assert_eq!(replay.stop, Some(Stop::AuthExpired));
        let outcome = RunOutcome::stopped(&[http, page, replay]).unwrap();
        assert!(
            matches!(
                outcome,
                RunOutcome::Stopped {
                    stop: Stop::AuthExpired,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_replay_that_answered_with_a_changed_shape_is_drift_not_a_soft_stop() {
        let mut req = signed_request("s1r1");
        req.expect.signature = signature(&json!({ "ok": true }));
        let http = denied_attempt(&req, Tier::Http, 401);
        let page = denied_attempt(&req, Tier::Page, 401);
        let replay = tier_result(
            &req,
            Tier::Replay,
            Instant::now(),
            Ok((200, r#"{"moved":1}"#.to_string())),
        );
        assert!(replay.shape_changed, "{:?}", replay.reason);
        let results = [http, page, replay];
        assert_eq!(RunOutcome::stopped(&results), None);
        let flow = flow_of(json!({}));
        assert!(matches!(
            RunOutcome::failed(&results, &flow),
            RunOutcome::Drift {
                shape_changed: true,
                ..
            }
        ));
    }

    #[test]
    fn a_transport_error_does_not_move_the_preferred_tier() {
        let mut flow = flow_with_request(None, false);
        let transport = TierResult {
            transport: true,
            ..result(Tier::Http, false, None, false, None)
        };
        let page = result(Tier::Page, true, Some(200), false, None);
        finish_run(
            &mut flow,
            &[transport.clone(), page.clone()],
            "now",
            1,
            &Shape::default(),
        );
        assert_eq!(flow.steps[0].requests[0].tier, None);
        // A failure that says the site changed does move it.
        let drifted = result(Tier::Http, false, Some(200), true, None);
        finish_run(&mut flow, &[drifted, page], "now", 1, &Shape::default());
        assert_eq!(flow.steps[0].requests[0].tier, Some(Tier::Page));
        // A transport error on the preferred tier keeps that preference.
        let replay = result(Tier::Replay, true, Some(200), false, None);
        let transport = TierResult {
            transport: true,
            ..result(Tier::Page, false, None, false, None)
        };
        finish_run(&mut flow, &[transport, replay], "now", 1, &Shape::default());
        assert_eq!(flow.steps[0].requests[0].tier, Some(Tier::Page));
    }

    #[test]
    fn a_replay_denial_ignores_the_recorded_credential_and_signature_headers() {
        let mut req = signed_request("s1r1");
        req.expect.stripped_auth = true;
        for status in [401, 403] {
            let r = denied_attempt(&req, Tier::Replay, status);
            assert_eq!(r.stop, Some(Stop::AuthExpired), "{status}");
        }
    }

    #[test]
    fn the_run_loop_lets_the_replay_answer_a_signed_request() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s1r1", "writes": [],
            "steps": [{ "command": {}, "requests": [{
                "id": "s1r1", "method": "GET", "url": "https://a.test/x",
                "headers": { "X-Ts": "1730000000" }, "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::json(401, "{}"))
            .answer(Tier::Page, "s1r1", Scripted::json(401, "{}"))
            .answer(Tier::Replay, "s1r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert!(results.last().unwrap().ok);
        assert_eq!(RunOutcome::stopped(&results), None);
        assert_eq!(senders.times("s1r1"), 3);
    }

    #[test]
    fn a_failure_is_judged_by_its_own_request() {
        let flow = flow_of(json!({ "params": [{ "name": "q", "example": "kettle" }] }));
        let mut r1_http = attempt_result(Tier::Http, Some(200), "shape", true, None, None);
        r1_http.shape_changed = true;
        let r1_page = result(Tier::Page, true, Some(200), false, None);
        let mut r2_http = attempt_result(Tier::Http, Some(404), "HTTP 404", false, None, None);
        r2_http.id = "s1r2".to_string();
        let outcome = RunOutcome::failed(&[r1_http, r1_page, r2_http], &flow);
        assert!(
            matches!(
                outcome,
                RunOutcome::Failed {
                    examples: Some(_),
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn a_read_falls_through_after_drift_a_transport_error_or_a_5xx_after_drift() {
        use Tier::*;
        let flow = flow_with_request(None, false);
        let req = prepared("s1r1");
        let transport = TierResult {
            transport: true,
            ..result(Http, false, None, false, None)
        };
        let cases: Vec<(Vec<TierResult>, Option<Tier>)> = vec![
            (vec![transport.clone()], Some(Page)),
            (
                vec![
                    result(Http, false, Some(200), true, None),
                    result(Page, false, Some(500), false, None),
                ],
                Some(Replay),
            ),
            (vec![result(Http, false, Some(500), false, None)], None),
            (vec![result(Http, false, Some(404), false, None)], None),
            (
                vec![
                    result(Http, false, Some(200), true, None),
                    result(Page, false, Some(404), false, None),
                ],
                None,
            ),
        ];
        for (i, (attempts, want)) in cases.into_iter().enumerate() {
            let got = next_tier(&flow, &req, &attempts, &WriteLedger::default());
            assert_eq!(got, want, "case {i}");
        }
    }

    #[test]
    fn a_write_that_may_have_been_sent_is_not_retried_after_a_transport_error() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s1r1",
            "writes": [{ "id": "s1r1", "method": "POST", "kind": "explicit" }],
            "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "POST", "url": "https://a.test/cart", "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::unknown())
            .answer(Tier::Page, "s1r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert_eq!(senders.times("s1r1"), 1);
        assert_eq!(results.last().unwrap().stop, Some(Stop::OutcomeUnknown));
    }

    #[test]
    fn the_run_loop_reads_through_a_transport_error() {
        use memory::{MemorySenders, Scripted};
        let (flow, requests) = run_flow(json!({
            "output": "s1r1", "writes": [],
            "steps": [{ "command": {}, "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x", "expect": ok_expect() }] }]
        }));
        let mut senders = MemorySenders::default()
            .answer(Tier::Http, "s1r1", Scripted::transport())
            .answer(Tier::Page, "s1r1", Scripted::json(200, OK_JSON));
        let results = block_on(run_requests(&flow, &requests, &mut senders, &|_| Ok(()))).unwrap();
        assert!(results.last().unwrap().ok);
        assert_eq!(senders.times("s1r1"), 2);
    }

    #[test]
    fn a_run_that_stops_after_a_shape_change_marks_the_flow_unverified() {
        let mut flow = flow_of(json!({
            "name": "f", "origin": "site.example", "output": "s1r1",
            "steps": [{ "requests": [{ "id": "s1r1", "method": "GET", "url": "https://a.test/x" }] }],
        }));
        flow.verified_at = Some("then".to_string());
        let mut http = attempt_result(Tier::Http, Some(200), "shape", true, None, None);
        http.shape_changed = true;
        let limited = attempt_result(
            Tier::Page,
            Some(429),
            "HTTP 429",
            false,
            Some(Stop::RateLimited),
            None,
        );
        assert!(!unverify_on_shape_change(
            &mut flow,
            std::slice::from_ref(&limited)
        ));
        assert!(flow.verified_at.is_some());
        assert!(unverify_on_shape_change(&mut flow, &[http, limited]));
        assert_eq!(flow.verified_at, None);
    }
}
