//! Saved flows: record a task once in the browser, then replay its
//! first-party API requests directly over HTTP.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::actions::HarEntry;

mod model;
pub use model::*;

/// Values `--capture` accepts.
pub const CAPTURE_MODES: &[&str] = &["flow"];

/// Check a `--capture` value list; only `flow` is supported.
pub fn parse_capture(values: &[String]) -> Result<(), String> {
    for v in values {
        if !CAPTURE_MODES.contains(&v.as_str()) {
            return Err(format!(
                "unknown --capture value '{}'. Supported: {}",
                v,
                CAPTURE_MODES.join(", ")
            ));
        }
    }
    Ok(())
}

/// What the recorder knows about a command besides its arguments.
#[derive(Clone, Default)]
pub struct StepMeta {
    pub ok: bool,
    /// Page URL before the command ran.
    pub from_url: Option<String>,
    /// Page URL after the command finished.
    pub url: Option<String>,
}

/// Commands and request tags recorded while the session runs with
/// `--capture flow`.
#[derive(Default)]
pub struct FlowRecorder {
    capturing: bool,
    suspended: bool,
    pub commands: Vec<(u64, Value)>,
    pub meta: HashMap<u64, StepMeta>,
    /// HAR request id to the sequence number of the command that was
    /// running when the request started.
    pub request_tags: HashMap<String, u64>,
    pub current_seq: Option<u64>,
    /// A paste whose value is read once the command has finished.
    pub paste_pending: Option<u64>,
    /// Steps that put a value of unknown content into a secret field.
    pub unknown_secret_seqs: Vec<u64>,
    /// Keys pressed so far in the secret field that has focus.
    key_buffer: String,
    /// The last entry of `secrets` is the current `key_buffer`.
    key_active: bool,
    /// A typed step saved as a param before its field could be read.
    pub needs_traits: Option<u64>,
    /// Sequence number the command being executed was recorded under.
    pub begun: Option<u64>,
    /// Values typed into secret fields, kept in memory only, so save can
    /// drop any captured request that carries one.
    pub secrets: Vec<String>,
    /// The tab's URL when capture began, for flows that start on an open page.
    pub page_url: Option<String>,
    /// Values typed into fields that are not secrets, as (param name, value, seq).
    pub typed: Vec<(String, String, u64)>,
    /// Sequence number of the last step covered by a save.
    pub last_save_seq: u64,
    /// Flows saved during this session, as (origin, name).
    pub saved: Vec<(String, String)>,
    next_seq: u64,
}

impl FlowRecorder {
    pub fn active(&self) -> bool {
        self.capturing
    }

    pub fn begin_capture(&mut self, page_url: Option<String>) {
        *self = FlowRecorder {
            capturing: true,
            page_url,
            ..FlowRecorder::default()
        };
    }

    /// Stop recording and report the steps no save covers.
    pub fn end_capture(&mut self) -> usize {
        let unsaved = self.unsaved_steps();
        *self = FlowRecorder::default();
        unsaved
    }

    pub fn unsaved_steps(&self) -> usize {
        self.commands
            .iter()
            .filter(|(seq, _)| *seq > self.last_save_seq)
            .count()
    }

    /// Commands run by a replay are not part of the recording.
    pub fn set_suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
    }

    /// Record a command and return its sequence number. The step counts as
    /// failed until `finish_command` says otherwise.
    pub fn records(&self, action: &str) -> bool {
        self.active() && !self.suspended && is_recordable(action)
    }

    pub fn begin_command(
        &mut self,
        action: &str,
        cmd: &Value,
        from_url: Option<String>,
        traits: Option<&FieldTraits>,
    ) -> Option<u64> {
        if !self.active() || self.suspended || !is_recordable(action) {
            return None;
        }
        self.next_seq += 1;
        let mut recorded = cmd.clone();
        if let Some(obj) = recorded.as_object_mut() {
            obj.remove("id");
        }
        let is_key = matches!(action, "press" | "keydown" | "keyup");
        if !is_key {
            self.reset_keys();
        }
        if let Some(secret) = redact_secret(&mut recorded, traits) {
            self.secrets.push(secret);
        } else if is_key {
            self.record_key(action, &mut recorded, traits);
        } else if action == "clipboard" {
            self.record_clipboard(&mut recorded, traits);
        } else if let Some(field) = typed_field(&recorded) {
            self.keep_typed_as_param(&mut recorded, field, traits);
        }
        self.commands.push((self.next_seq, recorded));
        self.meta.insert(
            self.next_seq,
            StepMeta {
                ok: false,
                from_url,
                url: None,
            },
        );
        self.current_seq = Some(self.next_seq);
        self.begun = Some(self.next_seq);
        Some(self.next_seq)
    }

    /// A value typed into an ordinary field is saved as a param, so the flow
    /// file carries a placeholder in the step and the value only as the
    /// param's recorded example.
    fn keep_typed_as_param(&mut self, cmd: &mut Value, field: &str, traits: Option<&FieldTraits>) {
        let Some(value) = cmd[field]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(String::from)
        else {
            return;
        };
        let name = match self.typed.iter().find(|(_, v, _)| *v == value) {
            Some((name, _, _)) => name.clone(),
            None => {
                let taken: Vec<String> = self.typed.iter().map(|(n, _, _)| n.clone()).collect();
                typed_param_name(traits, &taken)
            }
        };
        if traits.is_none() {
            self.needs_traits = Some(self.next_seq);
        }
        self.typed.push((name.clone(), value, self.next_seq));
        cmd[field] = json!(format!("{{{{{}}}}}", name));
    }

    /// A typed command whose field could not be read beforehand: once it has
    /// run, the focused element says whether it was a secret.
    pub fn redact_if_secret(&mut self, seq: u64, traits: &FieldTraits) {
        if self.needs_traits != Some(seq) {
            return;
        }
        self.needs_traits = None;
        let Some(i) = self.commands.iter().position(|(s, _)| *s == seq) else {
            return;
        };
        let Some(field) = typed_field(&self.commands[i].1) else {
            return;
        };
        if !traits.looks_secret() {
            return;
        }
        let Some(pos) = self.typed.iter().position(|(_, _, s)| *s == seq) else {
            return;
        };
        let (_, value, _) = self.typed.remove(pos);
        let cmd = &mut self.commands[i].1;
        cmd[field] = json!(REDACTED);
        cmd["redacted"] = json!(true);
        if !value.is_empty() {
            self.secrets.push(value);
        }
    }

    pub fn finish_command(&mut self, seq: u64, ok: bool, url: Option<String>) {
        if let Some(m) = self.meta.get_mut(&seq) {
            m.ok = ok;
            m.url = url;
        }
    }

    pub fn tag_request(&mut self, request_id: &str) {
        if self.suspended {
            return;
        }
        if let Some(seq) = self.current_seq {
            self.request_tags.insert(request_id.to_string(), seq);
        }
    }

    /// The first and last step a save covers. `from` is a sequence number,
    /// `open` (the last navigation) or `last-save`; without it the save
    /// starts at the last navigation after the previous save.
    pub fn resolve_range(&self, from: Option<&str>, to: Option<u64>) -> Result<(u64, u64), String> {
        let last = self.next_seq;
        if last == 0 {
            return Err("nothing recorded yet; run commands first, then `flow save`".to_string());
        }
        let to = to.unwrap_or(last);
        if to == 0 || to > last {
            return Err(format!(
                "--to {} is outside the recorded steps (1-{})",
                to, last
            ));
        }
        let navigations: Vec<u64> = self
            .commands
            .iter()
            .filter(|(seq, c)| {
                *seq <= to && c.get("action").and_then(|a| a.as_str()) == Some("navigate")
            })
            .map(|(seq, _)| *seq)
            .collect();
        let after_save = self.last_save_seq + 1;
        let from = match from {
            None => navigations
                .iter()
                .rev()
                .find(|seq| **seq > self.last_save_seq)
                .copied()
                .unwrap_or(after_save),
            Some("open") => navigations
                .last()
                .copied()
                .ok_or("no `open <url>` step to start from; pass --from <seq>")?,
            Some("last-save") => after_save,
            Some(n) => n.parse::<u64>().map_err(|_| {
                format!(
                    "invalid --from '{}': use a step number, `open` or `last-save`",
                    n
                )
            })?,
        };
        if from == 0 || from > to {
            return Err(format!(
                "no recorded steps from {} to {}; run `flow steps` to see them",
                from, to
            ));
        }
        Ok((from, to))
    }

    /// A command that is not recorded starts: requests it sends belong to no
    /// step, so a later save does not keep them.
    pub fn untag_current(&mut self) {
        self.current_seq = None;
        self.reset_keys();
    }

    pub fn add_secret(&mut self, value: &str) {
        if !value.is_empty() && !self.secrets.iter().any(|s| s == value) {
            self.secrets.push(value.to_string());
        }
    }

    fn reset_keys(&mut self) {
        self.key_buffer.clear();
        self.key_active = false;
    }

    fn set_key_buffer(&mut self, buffer: String) {
        if self.key_active {
            self.secrets.pop();
        }
        self.key_buffer = buffer;
        self.key_active = !self.key_buffer.is_empty();
        if self.key_active {
            self.secrets.push(self.key_buffer.clone());
        }
    }

    /// A key sent to a secret field is part of a secret: it is redacted from
    /// the step and joined to the keys before it, so a request carrying the
    /// whole value is dropped at save.
    fn record_key(&mut self, action: &str, cmd: &mut Value, traits: Option<&FieldTraits>) {
        if !traits.is_some_and(FieldTraits::looks_secret) {
            self.reset_keys();
            return;
        }
        let Some(key) = cmd["key"].as_str().map(String::from) else {
            return;
        };
        let (mods, last): (Vec<String>, String) = if key == "+" {
            (Vec::new(), key.clone())
        } else {
            let mut parts: Vec<&str> = key.split('+').collect();
            let last = parts.pop().unwrap_or("").to_string();
            (parts.iter().map(|m| m.to_ascii_lowercase()).collect(), last)
        };
        // Only a bare key or Shift plus a key adds a character.
        let plain = mods.iter().all(|m| m == "shift");
        let has = |names: &[&str]| mods.iter().any(|m| names.contains(&m.as_str()));
        let paste = (has(&["control", "ctrl", "meta", "command", "cmd"])
            && last.eq_ignore_ascii_case("v"))
            || (has(&["shift"]) && last.eq_ignore_ascii_case("insert"));
        if plain && last.chars().count() == 1 {
            if action != "keyup" {
                let mut buffer = self.key_buffer.clone();
                buffer.push_str(&last);
                self.set_key_buffer(buffer);
            }
            cmd["key"] = json!(REDACTED);
            cmd["redacted"] = json!(true);
        } else if plain && last == "Backspace" {
            if action != "keyup" {
                let mut buffer = self.key_buffer.clone();
                buffer.pop();
                self.set_key_buffer(buffer);
            }
        } else if paste {
            // The pasted value is unknown, so the step is redacted and the
            // requests it tagged are not saved.
            if action != "keyup" {
                self.unknown_secret_seqs.push(self.next_seq);
                self.paste_pending = Some(self.next_seq);
                self.reset_keys();
            }
            cmd["key"] = json!(REDACTED);
            cmd["redacted"] = json!(true);
        } else if action != "keyup" {
            self.reset_keys();
        }
    }

    /// `clipboard write` text is a secret; a `clipboard paste` into a secret
    /// field puts an unknown value there.
    fn record_clipboard(&mut self, cmd: &mut Value, traits: Option<&FieldTraits>) {
        match cmd["operation"].as_str() {
            Some("write") => {
                if let Some(text) = cmd["text"].as_str().map(String::from) {
                    cmd["text"] = json!(REDACTED);
                    cmd["redacted"] = json!(true);
                    self.add_secret(&text);
                }
            }
            Some("paste") if traits.is_some_and(FieldTraits::looks_secret) => {
                cmd["redacted"] = json!(true);
                self.unknown_secret_seqs.push(self.next_seq);
                self.paste_pending = Some(self.next_seq);
                self.reset_keys();
            }
            _ => {}
        }
    }

    /// The paste step has finished: the field's value, read from the page,
    /// becomes a secret in full (any typed prefix included) and later keys
    /// extend it. When the value cannot be read the step stays unknown and
    /// the requests it tagged are dropped.
    pub fn resolve_paste(&mut self, seq: u64, value: Option<String>) {
        if self.paste_pending != Some(seq) {
            return;
        }
        self.paste_pending = None;
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            self.unknown_secret_seqs.retain(|s| *s != seq);
            self.reset_keys();
            self.set_key_buffer(value);
        }
    }

    pub fn mark_saved(&mut self, last: u64) {
        self.last_save_seq = self.last_save_seq.max(last);
    }

    pub fn is_ok(&self, seq: u64) -> bool {
        self.meta.get(&seq).is_some_and(|m| m.ok)
    }
}

/// What the page says about the field a command types into.
#[derive(Clone, Default, Debug)]
pub struct FieldTraits {
    /// The `type` property: `password`, `text`, `email`, ...
    pub input_type: String,
    /// The `autocomplete` attribute.
    pub autocomplete: String,
    /// Name, id, aria-label, placeholder and label text, space separated.
    pub hint: String,
}

impl FieldTraits {
    pub fn from_value(v: &Value) -> FieldTraits {
        let text = |k: &str| v[k].as_str().unwrap_or("").to_string();
        FieldTraits {
            input_type: text("type"),
            autocomplete: text("autocomplete"),
            hint: text("hint"),
        }
    }

    /// A credential field by its type or autocomplete, or by what it is called.
    fn looks_secret(&self) -> bool {
        self.is_secret() || has_secret_marker(&self.hint)
    }

    /// Fields whose value is a credential, whatever the page calls them.
    fn is_secret(&self) -> bool {
        let ac = self.autocomplete.trim().to_ascii_lowercase();
        self.input_type.eq_ignore_ascii_case("password")
            || ac.split_whitespace().any(|t| {
                matches!(t, "current-password" | "new-password" | "one-time-code")
                    || t.starts_with("cc-")
            })
    }
}

fn has_secret_marker(text: &str) -> bool {
    let text = text.to_lowercase();
    SECRET_MARKERS.iter().any(|m| text.contains(m))
}

const SECRET_MARKERS: &[&str] = &[
    "pass",
    "contraseña",
    "secret",
    "token",
    "otp",
    "pin",
    "cvv",
    "card",
];

/// The field a command stores typed input in: `fill` and `find ... fill` use
/// `value`, `type` uses `text`.
fn typed_field(cmd: &Value) -> Option<&'static str> {
    let action = cmd["action"].as_str().unwrap_or("");
    let sub = cmd["subaction"].as_str().unwrap_or("");
    let kind = if matches!(action, "fill" | "type" | "inserttext") {
        action
    } else {
        sub
    };
    let field = match kind {
        "fill" => "value",
        "type" | "inserttext" | "insertText" => "text",
        _ => return None,
    };
    cmd[field].as_str().map(|_| field)
}

/// A value typed into a field that looks like a password or code is never
/// stored; the flow keeps a marker and cannot be replayed through the UI.
/// Returns the removed value. `fill` stores input in `value`, `type` in `text`.
fn redact_secret(cmd: &mut Value, traits: Option<&FieldTraits>) -> Option<String> {
    let typed = typed_field(cmd);
    let obj = cmd.as_object_mut()?;
    let field = match typed {
        Some(f) => f,
        None => ["value", "text"]
            .into_iter()
            .find(|k| obj.get(*k).and_then(|v| v.as_str()).is_some())?,
    };
    let mut target = ["selector", "name", "label", "placeholder"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(t) = traits {
        target.push(' ');
        target.push_str(&t.hint);
    }
    let target = target.to_lowercase();
    if !(traits.is_some_and(|t| t.is_secret()) || has_secret_marker(&target)) {
        return None;
    }
    let original = obj.get(field)?.as_str()?.to_string();
    obj.insert(field.to_string(), json!(REDACTED));
    obj.insert("redacted".to_string(), json!(true));
    (!original.is_empty()).then_some(original)
}

/// A param name from what the page calls the field: lowercase letters,
/// digits and underscores, starting with a letter.
fn typed_param_name(traits: Option<&FieldTraits>, taken: &[String]) -> String {
    let base: String = traits
        .and_then(|t| t.hint.split_whitespace().next())
        .map(|w| {
            w.chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        })
        .map(|w| w.trim_matches('_').chars().take(24).collect::<String>())
        .filter(|w| w.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
        .unwrap_or_else(|| "typed".to_string());
    let mut name = base.clone();
    let mut n = 1;
    while taken.contains(&name) {
        n += 1;
        name = format!("{}{}", base, n);
    }
    name
}

pub const REDACTED: &str = "<redacted>";

fn is_recordable(action: &str) -> bool {
    !(action.is_empty()
        || action.starts_with("flow_")
        || action.starts_with("har_")
        || action.starts_with("auth_")
        || action.starts_with("credentials")
        || matches!(action, "cookies_set" | "storage_set")
        || matches!(
            action,
            "launch" | "close" | "session_info" | "requests" | "request_detail" | "cookies_get"
        ))
}

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
        _ => HeaderClass::Kept,
    }
}

/// Registrable part of a host, close enough for first-party checks:
/// `api.example.com.ar` -> `example.com.ar`, `www.example.org` -> `example.org`.
pub fn base_domain(host: &str) -> String {
    let host = host.trim_end_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host.to_string();
    }
    // The public suffix list keeps `a.vercel.app` and `b.vercel.app` apart.
    psl::domain_str(&host.to_ascii_lowercase())
        .map(String::from)
        .unwrap_or_else(|| host.to_ascii_lowercase())
}

/// Hosts a flow may talk to: the base domain of its start URL.
pub fn derive_allowed_hosts(start_url: &str) -> Vec<String> {
    host_of(start_url)
        .map(|h| vec![base_domain(&h)])
        .unwrap_or_default()
}

fn host_matches(host: &str, allowed: &str) -> bool {
    if allowed.parse::<std::net::IpAddr>().is_ok() {
        return host == allowed;
    }
    host == allowed || host.ends_with(&format!(".{}", allowed))
}

/// Reject a URL outside the hosts a flow was recorded on. The start URL's
/// base domain is always required; `allowedHosts` in the file can only narrow it.
pub fn check_allowed_host(flow: &Flow, url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| format!("'{}' is not a valid URL", url))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("'{}' is not an http(s) URL", url));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("'{}' has no host", url))?
        .to_ascii_lowercase();
    let base = derive_allowed_hosts(&flow.start_url);
    let in_base = base.iter().any(|a| host_matches(&host, a));
    let listed = &flow.allowed_hosts;
    let in_listed = listed.is_empty() || listed.iter().any(|a| host_matches(&host, a));
    if in_base && in_listed {
        Ok(())
    } else {
        Err(format!(
            "{} is outside the hosts this flow may reach ({})",
            host,
            if base.is_empty() {
                "none".to_string()
            } else {
                base.join(", ")
            }
        ))
    }
}

/// A flow origin becomes a directory name, so it must be a plain hostname.
pub fn validate_origin(origin: &str) -> Result<(), String> {
    let ok = !origin.is_empty()
        && origin.len() <= 253
        && !origin.starts_with('.')
        && !origin.ends_with('.')
        && !origin.contains("..")
        && origin
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid flow origin '{}': expected a hostname",
            origin
        ))
    }
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(String::from)
}

fn is_first_party(url: &str, base: &str) -> bool {
    host_of(url)
        .map(|h| h == base || h.ends_with(&format!(".{}", base)))
        .unwrap_or(false)
}

fn is_telemetry(url: &str) -> bool {
    let path = url::Url::parse(url)
        .map(|u| u.path().to_ascii_lowercase())
        .unwrap_or_default();
    TELEMETRY_PATH_MARKERS
        .iter()
        .any(|m| path == *m || path.starts_with(&format!("{}/", m)) || path.ends_with(m))
}

fn is_api_entry(e: &HarEntry, base: &str) -> bool {
    matches!(e.resource_type.as_str(), "XHR" | "Fetch")
        && e.mime_type.contains("json")
        && e.status.map(|s| (200..300).contains(&s)).unwrap_or(false)
        && is_first_party(&e.url, base)
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

/// Like `signature_matches`, but a recorded list, and any count, may be
/// missing: many APIs drop an empty list and its totals instead of sending `[]`.
fn matches_without_lists(recorded: &Value, current: &Value) -> bool {
    matches_at(recorded, current, "", true).is_ok()
}

/// Paths of lists in `current` that the recording did not have.
fn new_lists(recorded: &Value, current: &Value, path: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(cur) = current.get("object").and_then(|v| v.as_object()) {
        let rec = recorded.get("object").and_then(|v| v.as_object());
        for (key, sig) in cur {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path, key)
            };
            match rec.and_then(|r| r.get(key)) {
                None if signature_type(sig) == "array" => out.push(child),
                None => {}
                Some(rec_sig) => out.extend(new_lists(rec_sig, sig, &child)),
            }
        }
    }
    out
}

fn matches_at(
    recorded: &Value,
    current: &Value,
    path: &str,
    lists_optional: bool,
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
                // Counts and totals often go with the list.
                None if lists_optional && matches!(signature_type(sig), "array" | "number") => {}
                None => return Err(format!("key '{}' is missing", child)),
                Some(cur_sig) => matches_at(sig, cur_sig, &child, lists_optional)?,
            }
        }
    }
    if let (Some(rec), Some(cur)) = (recorded.get("array"), current.get("array")) {
        if !rec.is_null() && !cur.is_null() {
            matches_at(rec, cur, &format!("{}[]", path), lists_optional)?;
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

fn templatize(text: &str, params: &[(String, String)], encoded: bool) -> String {
    let mut out = text.to_string();
    for (name, value) in params {
        if value.is_empty() {
            continue;
        }
        let needle = if encoded {
            urlencoding::encode(value).into_owned()
        } else {
            value.clone()
        };
        out = out.replace(&needle, &format!("{{{{{}}}}}", name));
        if encoded && needle != *value {
            out = out.replace(value.as_str(), &format!("{{{{{}}}}}", name));
        }
    }
    out
}

fn fill(template: &str, params: &HashMap<String, String>, encoded: bool) -> Result<String, String> {
    let mut out = template.to_string();
    for (name, value) in params {
        let v = if encoded {
            urlencoding::encode(value).into_owned()
        } else {
            value.clone()
        };
        out = out.replace(&format!("{{{{{}}}}}", name), &v);
    }
    if let Some(start) = out
        .match_indices("{{")
        .map(|(i, _)| i)
        .find(|i| !out[*i..].starts_with("{{@"))
    {
        if let Some(len) = out[start..].find("}}") {
            return Err(format!("missing --param {}", &out[start + 2..start + len]));
        }
    }
    Ok(out)
}

/// Apply `f` to every string in a JSON body and serialize it again, so a
/// value with quotes or backslashes stays valid JSON. Bodies that are not
/// JSON go through `f` as plain text.
fn map_body(body: &str, f: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    match serde_json::from_str::<Value>(body) {
        Ok(mut v) => {
            map_strings(&mut v, f)?;
            serde_json::to_string(&v).map_err(|e| e.to_string())
        }
        Err(_) => f(body),
    }
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

fn carries_secret(e: &HarEntry, secrets: &[String]) -> bool {
    secrets.iter().any(|secret| {
        let encoded = urlencoding::encode(secret);
        let escaped = serde_json::to_string(secret).unwrap_or_default();
        let escaped = escaped.trim_matches('"');
        let found = |text: &str| {
            text.contains(secret.as_str())
                || text.contains(encoded.as_ref())
                || text.contains(escaped)
                || decoded_forms(text)
                    .iter()
                    .any(|d| d.contains(secret.as_str()))
        };
        found(&e.url)
            || e.post_data.as_deref().is_some_and(found)
            || e.request_headers.iter().any(|(_, v)| found(v))
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
    base: &str,
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
        if is_api_entry(e, base) {
            if let Ok(parsed) = serde_json::from_str::<Value>(body) {
                out.push((e, parsed, None));
            }
        } else if is_document_entry(e, base) {
            for (spec, parsed) in embedded_json(body) {
                out.push((e, parsed, Some(spec)));
            }
        }
    }
    out
}

fn is_document_entry(e: &HarEntry, base: &str) -> bool {
    e.resource_type == "Document"
        && e.method == "GET"
        && e.mime_type.contains("html")
        && e.status.map(|s| (200..300).contains(&s)).unwrap_or(false)
        && is_first_party(&e.url, base)
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
    base: &str,
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
            (is_api_entry(e, base)
                || (is_document_entry(e, base)
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
    let given = with_page_query_params(&start, &base, &explicit_and_typed, input.entries);

    let mut steps = Vec::new();
    let mut gaps = Vec::new();
    let mut dropped_headers: Vec<String> = Vec::new();
    let mut request_ids: Vec<String> = Vec::new();
    let mut bodies: Vec<(String, Value)> = Vec::new();
    let mut secret_requests: Vec<String> = Vec::new();
    let mut har_ids: HashMap<String, String> = HashMap::new();

    for (index, (seq, cmd)) in commands.iter().enumerate() {
        let tagged: Vec<&HarEntry> = input
            .entries
            .iter()
            .filter(|e| input.recorder.request_tags.get(&e.request_id) == Some(seq))
            .collect();
        let mut requests = Vec::new();
        let mut seen = Vec::new();
        for (e, parsed, extract) in candidates(&tagged, &base) {
            if carries_secret(e, &input.recorder.secrets)
                || input.recorder.unknown_secret_seqs.contains(seq)
            {
                if !secret_requests.contains(&e.request_id) {
                    secret_requests.push(e.request_id.clone());
                    gaps.push(json!({
                        "step": index + 1,
                        "command": describe_command(cmd),
                        "reason": "a request may carry a value typed or pasted into a secret field, so it was not saved",
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
            let mut headers = Map::new();
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
                headers.insert(k.clone(), json!(templatize(v, &given, false)));
            }
            let item_fields = item_fields(&parsed);
            let id = format!("s{}r{}", index + 1, requests.len() + 1);
            // A write's response is not a source: the flow would have to send it.
            if !is_write(&e.method, e.post_data.as_deref()) {
                bodies.push((id.clone(), parsed.clone()));
            }
            request_ids.push(id.clone());
            har_ids.insert(id.clone(), e.request_id.clone());
            requests.push(json!({
                "id": id,
                "method": e.method,
                "url": templatize(&e.url, &given, true),
                "headers": headers,
                "body": e.post_data.as_deref().map(|b| templatize(b, &given, false)),
                "expect": {
                    "status": e.status,
                    "mime": e.mime_type,
                    "signature": signature(&parsed),
                    "bytes": body_len,
                    "itemFields": item_fields,
                    "extract": extract,
                    "strippedAuth": stripped_auth,
                },
            }));
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
            } else if tagged.iter().any(|e| is_first_party(&e.url, &base)) {
                "first-party traffic was documents or static files, not a JSON API"
            } else {
                "only third-party traffic"
            };
            gaps.push(
                json!({ "step": index + 1, "command": describe_command(cmd), "reason": reason }),
            );
        }
        steps.push(json!({
            "seq": seq,
            "ok": input.recorder.is_ok(*seq),
            "command": cmd,
            "requests": requests,
        }));
    }

    if request_ids.is_empty() {
        return Err(format!(
            "no first-party JSON requests were recorded for {}; this flow has nothing to replay over HTTP",
            base
        ));
    }
    let page_segments: Vec<String> = url::Url::parse(&start)
        .ok()
        .and_then(|u| u.path_segments().map(|p| p.map(String::from).collect()))
        .unwrap_or_default();
    let links = link_requests(&mut steps, &bodies, &har_ids, &page_segments, &given);
    let mut params: Vec<Value> = given
        .iter()
        .map(|(k, v)| {
            if input.params.iter().any(|(n, _)| n == k) {
                json!({ "name": k, "example": v, "source": "typed" })
            } else if typed.iter().any(|(n, _)| n == k) {
                json!({ "name": k, "source": "typed" })
            } else {
                json!({ "name": k, "example": v, "source": "query", "from": { "pageQuery": k } })
            }
        })
        .collect();
    for (name, _) in &typed {
        if !given.iter().any(|(n, _)| n == name) {
            params.push(json!({ "name": name, "source": "typed" }));
        }
    }
    params.extend(links.params.iter().cloned().map(|mut p| {
        p["source"] = json!("response");
        p
    }));
    let output = match input.output {
        Some(id) if request_ids.iter().any(|r| r == id) => id.to_string(),
        Some(id) => return Err(format!("unknown request id '{}' for --output", id)),
        None => default_output(&steps),
    };

    let flow_json = json!({
        "version": FLOW_VERSION,
        "name": input.name,
        "origin": host,
        "startUrl": start,
        "allowedHosts": derive_allowed_hosts(&start),
        "createdAt": input.created_at,
        "createdBy": { "tool": "agent-browser", "version": env!("CARGO_PKG_VERSION") },
        "description": input.description,
        "redactedSteps": commands
            .iter()
            .enumerate()
            .filter(|(_, (_, c))| c["redacted"] == true)
            .map(|(i, _)| i + 1)
            .collect::<Vec<_>>(),
        "params": params,
        "output": output,
        "itemFields": steps
            .iter()
            .flat_map(|s| s["requests"].as_array().into_iter().flatten())
            .find(|r| r["id"] == output.as_str())
            .map(|r| r["expect"]["itemFields"].clone())
            .unwrap_or(json!([])),
        "provenance": "observed",
        "writes": [],
        "verifiedAt": Value::Null,
        "steps": steps,
    });

    let mut flow: Flow = serde_json::from_value(flow_json)
        .map_err(|e| format!("the flow could not be built: {}", e))?;
    let consumed = dependencies(&flow, &output);
    let writes = collect_writes(&input, &base, &given, &har_ids, &consumed);
    for r in flow.steps.iter_mut().flat_map(|s| s.requests.iter_mut()) {
        r.write = writes.iter().any(|w| w.id == r.id);
    }
    flow.writes = writes;

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
        "params": params.iter().map(|p| p["name"].clone()).collect::<Vec<_>>(),
        "detectedParams": params
            .iter()
            .filter(|p| p.get("from").is_some())
            .cloned()
            .collect::<Vec<_>>(),
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
    params: Vec<Value>,
    /// Values that came from a single field of an earlier response: filled in
    /// at run time from that response.
    references: Vec<Value>,
}

/// Values a request sends: query values, path segments and JSON body scalars.
fn sent_values(req: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(url) = url::Url::parse(req["url"].as_str().unwrap_or("")) {
        out.extend(url.query_pairs().map(|(_, v)| v.into_owned()));
        if let Some(segs) = url.path_segments() {
            out.extend(segs.map(|s| {
                urlencoding::decode(s)
                    .map(|d| d.into_owned())
                    .unwrap_or_default()
            }));
        }
    }
    if let Some(body) = req["body"]
        .as_str()
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
    steps: &mut [Value],
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
    // Position of every request, writes included, so a write can take a value
    // from any response that came before it.
    let rank: HashMap<String, usize> = steps
        .iter()
        .flat_map(|s| s["requests"].as_array().into_iter().flatten())
        .filter_map(|r| r["id"].as_str())
        .enumerate()
        .map(|(i, id)| (id.to_string(), i))
        .collect();
    for step in steps.iter_mut() {
        let Some(requests) = step["requests"].as_array_mut() else {
            continue;
        };
        for req in requests.iter_mut() {
            let id = req["id"].as_str().unwrap_or("").to_string();
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
                let Some((from, path, in_list)) = earlier
                    .iter()
                    .rev()
                    .filter(|(from, _)| !same_response(from))
                    .find_map(|(from, body)| {
                        find_value(body, &value, "").map(|(p, l)| (from.clone(), p, l))
                    })
                else {
                    continue;
                };
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
                    let name = match links.params.iter().find(|p| p["example"] == value.as_str()) {
                        Some(p) => p["name"].as_str().unwrap_or("").to_string(),
                        None => {
                            let mut name = field.clone();
                            let mut n = 2;
                            while links.params.iter().any(|p| p["name"] == name.as_str())
                                || given.iter().any(|(k, _)| *k == name)
                            {
                                name = format!("{}{}", field, n);
                                n += 1;
                            }
                            links.params.push(json!({
                                "name": name,
                                "example": value,
                                "from": { "request": from, "list": path },
                            }));
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
                for key in ["url", "body"] {
                    if let Some(text) = req[key].as_str() {
                        let encoded = urlencoding::encode(&value).into_owned();
                        let replaced = replace_token(
                            &replace_token(text, &encoded, &placeholder),
                            &value,
                            &placeholder,
                        );
                        req[key] = json!(replaced);
                    }
                }
            }
        }
    }
    links
}

/// Values a reference placeholder in `text` points at, filled from the
/// responses already received in this run.
pub fn fill_references(
    text: &str,
    bodies: &HashMap<String, Value>,
    encoded: bool,
) -> Result<String, String> {
    let mut out = text.to_string();
    while let Some(start) = out.find("{{@") {
        let Some(len) = out[start..].find("}}") else {
            break;
        };
        let inner = out[start + 3..start + len].to_string();
        let (from, path) = inner
            .split_once('.')
            .ok_or(format!("bad reference {}", inner))?;
        let body = bodies
            .get(from)
            .ok_or(format!("reference to {} before it ran", from))?;
        let value = lookup(body, path)
            .map(as_text)
            .ok_or(format!("{} no longer has '{}'", from, path))?;
        let value = if encoded {
            urlencoding::encode(&value).into_owned()
        } else {
            value
        };
        out.replace_range(start..start + len + 2, &value);
    }
    Ok(out)
}

/// Request ids a request needs answers from, in run order.
pub fn dependencies(flow: &Flow, id: &str) -> Vec<String> {
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
fn default_output(steps: &[Value]) -> String {
    let requests: Vec<&Value> = steps
        .iter()
        .flat_map(|s| s["requests"].as_array().into_iter().flatten())
        .collect();
    let uses_param = |r: &&Value| {
        r["url"].as_str().unwrap_or("").contains("{{")
            || r["body"].as_str().unwrap_or("").contains("{{")
    };
    let pool: Vec<&Value> = if requests.iter().any(uses_param) {
        requests.iter().copied().filter(uses_param).collect()
    } else {
        requests
    };
    pool.iter()
        .max_by_key(|r| r["expect"]["bytes"].as_u64().unwrap_or(0))
        .and_then(|r| r["id"].as_str())
        .unwrap_or_default()
        .to_string()
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

fn valid_name(name: &str) -> Result<(), String> {
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

pub fn validate_name(name: &str) -> Result<(), String> {
    valid_name(name)
}

pub fn flow_path(origin: &str, name: &str) -> Result<PathBuf, String> {
    validate_origin(origin)?;
    valid_name(name)?;
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
    valid_name(name)?;
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
        .filter(|r| flow.output.is_empty() || needed.contains(&r.id))
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

    let mut out = Vec::new();
    for (index, step) in flow.steps.iter().enumerate() {
        for r in &step.requests {
            let headers = r
                .headers
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), fill(v, &params, false).ok()?)))
                .collect();
            out.push(PreparedRequest {
                id: r.id.clone(),
                step: index + 1,
                method: r.method.clone(),
                url: fill(&r.url, &params, true)?,
                headers,
                body: match &r.body {
                    Some(b) => Some(map_body(b, &|t| fill(t, &params, false))?),
                    None => None,
                },
                expect: r.expect.clone(),
                write: r.write,
            });
        }
    }
    Ok(out)
}

impl PreparedRequest {
    /// Fill reference placeholders from the responses received so far.
    pub fn resolve(&self, bodies: &HashMap<String, Value>) -> Result<PreparedRequest, String> {
        let mut out = self.clone();
        out.url = fill_references(&self.url, bodies, true)?;
        if let Some(b) = &self.body {
            out.body = Some(map_body(b, &|t| fill_references(t, bodies, false))?);
        }
        Ok(out)
    }
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

/// A command that acts on an element, as opposed to navigating or typing.
fn is_click_command(cmd: &Value) -> bool {
    let action = cmd["action"].as_str().unwrap_or("");
    let sub = cmd["subaction"].as_str().unwrap_or("");
    matches!(action, "click" | "dblclick")
        || (action == "getbyrole" && matches!(sub, "click" | "dblclick"))
}

/// The writes a recording caused, kept as requests or not: each one is
/// sent again by a run (or by the command replay) and must be allowed.
/// `suspected` marks GET requests that look like state changes: a data
/// request a click fired that no output depends on, or a path with a
/// mutating segment.
fn collect_writes(
    input: &SaveInput<'_>,
    base: &str,
    given: &[(String, String)],
    har_ids: &HashMap<String, String>,
    consumed: &[String],
) -> Vec<Write> {
    let mut writes: Vec<Write> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, (seq, cmd)) in input.commands.iter().enumerate() {
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
            let first_party = is_first_party(&e.url, base);
            let explicit = is_write(&e.method, e.post_data.as_deref());
            let suspected = !explicit
                && e.method == "GET"
                && first_party
                && (has_mutating_segment(&e.url)
                    || (is_click_command(cmd)
                        && matches!(e.resource_type.as_str(), "XHR" | "Fetch" | "Ping" | "Other")
                        && !kept.as_ref().is_some_and(|id| consumed.contains(id))));
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
                url: templatize(&e.url, given, true),
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

/// Ids of the writes a run sends over HTTP or from the page: the ones among
/// the requests the output needs. Each must be allowed or confirmed first.
pub fn request_write_ids(flow: &Flow) -> Vec<String> {
    let needed = dependencies(flow, &flow.output);
    flow.writes
        .iter()
        .filter(|w| needed.contains(&w.id))
        .map(|w| w.id.clone())
        .collect()
}

/// Writes the command replay would send beyond the ones a run sends itself.
pub fn replay_only_writes(flow: &Flow) -> Vec<Write> {
    let own = request_write_ids(flow);
    flow.writes
        .iter()
        .filter(|w| !own.contains(&w.id))
        .cloned()
        .collect()
}

/// Ids of every write the recording caused.
pub fn all_write_ids(flow: &Flow) -> Vec<String> {
    flow.writes.iter().map(|w| w.id.clone()).collect()
}

pub fn is_flow_write(flow: &Flow, id: &str) -> bool {
    flow.write(id).is_some()
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
}

pub fn write_policy_action(id: &str) -> String {
    format!("flow_write:{}", id)
}

/// Check that an id names a write of this flow.
pub fn check_write_ids(flow: &Flow, allow: &AllowWrite) -> Result<(), String> {
    for id in &allow.ids {
        if !is_flow_write(flow, id) {
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

pub fn is_write(method: &str, body: Option<&str>) -> bool {
    !matches!(method, "GET" | "HEAD" | "OPTIONS") && !is_graphql_query(body)
}

/// A GraphQL POST that only runs queries reads data; mutations still count
/// as writes.
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
                (q.starts_with("query") || q.starts_with('{')) && !q.contains("mutation")
            })
        })
}

pub struct TierResult {
    pub id: String,
    pub step: usize,
    pub ok: bool,
    pub status: Option<u16>,
    pub reason: Option<String>,
    pub body: Option<Value>,
    pub ms: u128,
    /// The failure says the site or its gate changed, not that the inputs were wrong.
    pub drift: bool,
    /// The response came back with a different shape than the recording.
    pub shape_changed: bool,
    /// The response matches the recording except that its lists are absent,
    /// which is how many APIs answer a query with no results.
    pub empty: bool,
    pub tier: Tier,
    /// The response says the run cannot go on, whatever tier sends it:
    /// `auth_expired`, `rate_limited` or `blocked`.
    pub kind: Option<&'static str>,
    pub retry_after: Option<String>,
}

impl TierResult {
    fn failed(req: &PreparedRequest, tier: Tier, ms: u128, f: Failure) -> TierResult {
        TierResult {
            id: req.id.clone(),
            step: req.step,
            ok: false,
            status: f.status,
            reason: Some(f.reason),
            body: None,
            ms,
            drift: f.drift,
            shape_changed: f.shape_changed,
            empty: false,
            tier,
            kind: f.kind,
            retry_after: None,
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    pub status: Option<u16>,
    pub reason: String,
    pub drift: bool,
    pub shape_changed: bool,
    pub kind: Option<&'static str>,
}

impl Failure {
    fn new(drift: bool, status: Option<u16>, reason: String) -> Failure {
        Failure {
            status,
            reason,
            drift,
            shape_changed: false,
            kind: None,
        }
    }

    /// A response that ends the run: another tier would get the same answer,
    /// and it does not mean the site changed.
    fn stop(kind: &'static str, status: u16, reason: String) -> Failure {
        Failure {
            status: Some(status),
            reason,
            drift: false,
            shape_changed: false,
            kind: Some(kind),
        }
    }
}

/// Challenge pages are HTML; a JSON body that mentions a captcha is data.
/// A write whose outcome is not known is never sent again by another tier.
fn unknown_outcome(reason: String) -> Failure {
    Failure {
        status: None,
        reason,
        drift: false,
        shape_changed: false,
        kind: Some("outcome_unknown"),
    }
}

fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start();
    head.starts_with('<') && serde_json::from_str::<Value>(body).is_err()
}

fn looks_like_challenge(body: &str) -> bool {
    let head: String = body
        .chars()
        .take(4000)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "cf-chl",
        "captcha",
        "challenge-platform",
        "px-captcha",
        "_incapsula_",
    ]
    .iter()
    .any(|m| head.contains(m))
}

/// Judge a response the same way on every tier: blocked, rejected input,
/// changed shape, or data.
pub fn check_response(
    req: &PreparedRequest,
    status: u16,
    text: &str,
) -> Result<(Value, bool), Failure> {
    if (matches!(status, 401 | 403 | 429) || (200..300).contains(&status))
        && looks_like_html(text)
        && looks_like_challenge(text)
    {
        return Err(Failure::stop(
            "blocked",
            status,
            "bot challenge page instead of data".to_string(),
        ));
    }
    match status {
        401 | 403 => {
            return Err(Failure::stop(
                "auth_expired",
                status,
                format!("HTTP {}: the session is signed out or lacks access", status),
            ))
        }
        429 => {
            return Err(Failure::stop(
                "rate_limited",
                status,
                "HTTP 429: the site is rate limiting this session".to_string(),
            ))
        }
        407 => return Err(Failure::new(true, Some(status), format!("HTTP {}", status))),
        _ => {}
    }
    if !(200..300).contains(&status) {
        return Err(Failure::new(
            false,
            Some(status),
            format!("HTTP {}", status),
        ));
    }
    let parsed: Value = serde_json::from_str(text)
        .map_err(|_| Failure::new(true, Some(status), "response is not JSON".to_string()))?;
    let current = signature(&parsed);
    if let Err(reason) = signature_matches(&req.expect.signature, &current) {
        // A list that only went missing reads as no results; a list that
        // appeared under a new name means the contract moved.
        if matches_without_lists(&req.expect.signature, &current)
            && new_lists(&req.expect.signature, &current, "").is_empty()
        {
            return Ok((parsed, true));
        }
        return Err(Failure {
            status: Some(status),
            reason: format!("response shape changed: {}", reason),
            drift: true,
            shape_changed: true,
            kind: None,
        });
    }
    Ok((parsed, false))
}

pub fn tier_result(
    req: &PreparedRequest,
    tier: Tier,
    started: Instant,
    outcome: Result<(u16, String), Failure>,
) -> TierResult {
    let ms = started.elapsed().as_millis();
    let outcome = match req.expect.extract.as_ref().filter(|x| !x.is_null()) {
        Some(spec) => outcome.and_then(|(status, text)| match extract_embedded(&text, spec) {
            Some(json) => Ok((status, json)),
            None if (200..300).contains(&status) => Err(Failure::new(
                true,
                Some(status),
                "the page no longer embeds the recorded data".to_string(),
            )),
            None => Ok((status, text)),
        }),
        None => outcome,
    };
    let checked = outcome
        .and_then(|(status, text)| check_response(req, status, &text).map(|body| (status, body)));
    match checked {
        Ok((status, (body, empty))) => TierResult {
            id: req.id.clone(),
            step: req.step,
            ok: true,
            status: Some(status),
            reason: None,
            body: Some(body),
            ms,
            drift: false,
            shape_changed: false,
            empty,
            tier,
            kind: None,
            retry_after: None,
        },
        Err(mut f) => {
            // The recording sent credentials a run cannot copy, so a 401 or
            // 403 may only mean this tier lacks them; the next tier sends
            // them from the page.
            if f.kind == Some("auth_expired") && tier != Tier::Replay && req.expect.stripped_auth {
                f.kind = None;
                f.drift = true;
            }
            TierResult::failed(req, tier, ms, f)
        }
    }
}

/// A session route (`network route`) that matches the request answers it on
/// the HTTP tier too, so mocks apply to the whole session.
pub struct RoutedResponse {
    pub abort: bool,
    pub status: u16,
    pub body: String,
}

/// Tier 1: send the request directly, with cookies from the live session.
pub async fn run_http(
    client: &reqwest::Client,
    req: &PreparedRequest,
    cookie_header: Option<&str>,
    routed: Option<RoutedResponse>,
) -> TierResult {
    let started = Instant::now();
    if let Some(r) = routed {
        let outcome = if r.abort {
            Err(Failure::new(
                false,
                None,
                "request aborted by a session route".to_string(),
            ))
        } else {
            Ok((r.status, r.body))
        };
        return tier_result(req, Tier::Http, started, outcome);
    }
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return tier_result(
                req,
                Tier::Http,
                started,
                Err(Failure::new(
                    true,
                    None,
                    format!("unsupported method {}", req.method),
                )),
            )
        }
    };
    let mut builder = client.request(method, &req.url);
    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }
    if let Some(cookie) = cookie_header.filter(|c| !c.is_empty()) {
        builder = builder.header("cookie", cookie);
    }
    // Browsers send Origin on every write; sites check it against CSRF.
    let has_origin = req
        .headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("origin"));
    if is_write(&req.method, req.body.as_deref()) && !has_origin {
        if let Some(origin) = req
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("referer"))
            .and_then(|(_, v)| url::Url::parse(v).ok())
            .map(|u| u.origin().ascii_serialization())
        {
            builder = builder.header("origin", origin);
        }
    }
    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }
    let mut retry_after = None;
    let outcome = match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            if resp.status().is_redirection() {
                let to = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                return tier_result(
                    req,
                    Tier::Http,
                    started,
                    Err(Failure {
                        status: Some(status),
                        reason: format!("HTTP {}: the redirect to {} was not followed", status, to),
                        drift: false,
                        shape_changed: false,
                        kind: None,
                    }),
                );
            }
            resp.text().await.map(|t| (status, t)).map_err(|e| {
                Failure::new(true, Some(status), format!("could not read body: {}", e))
            })
        }
        // A write that left the machine may have been applied.
        Err(e) if req.write && !e.is_connect() => Err(unknown_outcome(format!(
            "the write was sent but no response came back: {}",
            e
        ))),
        Err(e) => Err(Failure::new(false, None, format!("request failed: {}", e))),
    };
    with_retry_after(tier_result(req, Tier::Http, started, outcome), retry_after)
}

fn with_retry_after(mut result: TierResult, retry_after: Option<String>) -> TierResult {
    if result.kind == Some("rate_limited") {
        result.retry_after = retry_after.filter(|v| !v.trim().is_empty());
    }
    result
}

/// Tier 2: the page sends the request itself, so the browser adds its own
/// cookies, client hints and any headers the site's scripts would not.
pub fn page_fetch_script(req: &PreparedRequest) -> String {
    // Headers the browser sets itself. Sending them as app headers turns the
    // request into a CORS preflight the site never allowed.
    let headers: Map<String, Value> = req
        .headers
        .iter()
        .filter(|(k, _)| classify_header(k) != HeaderClass::BrowserSet)
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let mut init = json!({
        "method": req.method,
        "headers": headers,
        "body": req.body,
    });
    // A write must not follow a redirect to another site; the 3xx counts as
    // the site's answer.
    if req.write {
        init["redirect"] = json!("manual");
    }
    // Cross-origin APIs differ on credentials: some need cookies, some reject
    // a credentialed request outright. Try with, then without.
    // A write is never sent a second time: a thrown fetch may still have
    // reached the server, so it is reported as an unknown outcome instead.
    let send = if req.write {
        "const r = await fetch(u, { ...i, credentials: 'include' });"
    } else {
        "let r; try { r = await fetch(u, { ...i, credentials: 'include' }); } \
         catch (e) { r = await fetch(u, i); }"
    };
    format!(
        "(async () => {{ const u = {url}, i = {init}; \
         {send} \
         return {{ status: r.type === 'opaqueredirect' ? 302 : r.status, retryAfter: r.headers.get('retry-after'), text: await r.text() }}; }})()",
        url = json!(req.url),
        init = init,
        send = send
    )
}

/// The in-page tier runs only on a tab that is already on the flow's site.
pub fn page_skipped(req: &PreparedRequest, started: Instant, reason: String) -> TierResult {
    tier_result(
        req,
        Tier::Page,
        started,
        Err(Failure::new(true, None, reason)),
    )
}

pub fn page_result(
    req: &PreparedRequest,
    started: Instant,
    value: Result<Value, String>,
) -> TierResult {
    let retry_after = value
        .as_ref()
        .ok()
        .and_then(|v| v["retryAfter"].as_str())
        .map(String::from);
    let outcome = match value {
        Ok(v) => match (v["status"].as_u64(), v["text"].as_str()) {
            (Some(status), Some(text)) => Ok((status as u16, text.to_string())),
            _ => Err(Failure::new(
                true,
                None,
                "in-page fetch returned no response".to_string(),
            )),
        },
        Err(e) if req.write => Err(unknown_outcome(format!(
            "the in-page write may have been sent: {}",
            e
        ))),
        Err(e) => Err(Failure::new(
            true,
            None,
            format!("in-page fetch failed: {}", e),
        )),
    };
    with_retry_after(tier_result(req, Tier::Page, started, outcome), retry_after)
}

/// Tier 3: the recorded commands with the run's params swapped in. A param
/// whose recorded value no command contains cannot be replayed: the browser
/// would answer with the recorded value instead.
pub fn replay_commands(flow: &Flow, cmd_params: &[(String, String)]) -> Result<Vec<Value>, String> {
    let given = |name: &str| cmd_params.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let swaps: Vec<(String, String)> = flow
        .params
        .iter()
        .filter_map(|p| {
            let example = p.example.as_deref()?;
            let value = given(&p.name)?.clone();
            (!example.is_empty() && example != value).then(|| (example.to_string(), value))
        })
        .collect();
    // Typed values are stored as `{{name}}` and take the run's value, or the
    // recorded example when the run gives none.
    let placeholders: Vec<(String, String)> = flow
        .params
        .iter()
        .filter_map(|p| {
            let value = given(&p.name).cloned().or_else(|| p.example.clone())?;
            Some((format!("{{{{{}}}}}", p.name), value))
        })
        .collect();
    let recorded = serde_json::to_string(
        &flow
            .steps
            .iter()
            .map(|s| s.command.clone())
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    for p in &flow.params {
        if given(&p.name).is_none()
            && p.example.is_none()
            && recorded.contains(&format!("{{{{{}}}}}", p.name))
        {
            return Err(format!(
                "missing --param {}: the recording did not keep the typed value",
                p.name
            ));
        }
    }
    let in_commands = |text: &str| {
        recorded.contains(text) || recorded.contains(urlencoding::encode(text).as_ref())
    };
    if let Some((from, _)) = swaps.iter().find(|(from, _)| {
        let named = flow.params.iter().any(|p| {
            p.example.as_deref() == Some(from.as_str())
                && recorded.contains(&format!("{{{{{}}}}}", p.name))
        });
        !named && !in_commands(from)
    }) {
        let name = flow
            .params
            .iter()
            .find(|p| p.example.as_deref() == Some(from.as_str()))
            .map(|p| p.name.as_str())
            .unwrap_or("?");
        return Err(format!(
            "param '{}' is not in the recorded commands, so a replay would return the recorded value {}",
            name, from
        ));
    }
    if flow.steps.iter().any(|s| s.command["redacted"] == true) {
        return Err(
            "the recording typed a secret, which is not stored, so the commands cannot be replayed"
                .to_string(),
        );
    }
    let opens_page = flow.steps.iter().any(|s| s.command["action"] == "navigate");
    // A flow recorded on an already open page starts from that page.
    let start = (!opens_page).then(|| json!({ "action": "navigate", "url": flow.start_url }));
    Ok(start
        .into_iter()
        .chain(flow.steps.iter().filter(|s| s.ok).map(|s| {
            let mut cmd = s.command.clone();
            let _ = map_strings(&mut cmd, &|text| {
                let mut text = text.to_string();
                for (from, to) in &swaps {
                    text = text.replace(from.as_str(), to);
                    let (ef, et) = (urlencoding::encode(from), urlencoding::encode(to));
                    if ef != *from {
                        text = text.replace(ef.as_ref(), et.as_ref());
                    }
                }
                for (ph, value) in &placeholders {
                    text = text.replace(ph.as_str(), value);
                }
                Ok(text)
            });
            cmd
        }))
        .collect())
}

/// Commands a replay may send. Anything that runs page script, or that the
/// recorder does not know to be a plain interaction, is left out.
const REPLAY_ACTIONS: &[&str] = &[
    "navigate",
    "click",
    "dblclick",
    "fill",
    "type",
    "press",
    "select",
    "check",
    "uncheck",
    "hover",
    "focus",
    "scroll",
    "scrollintoview",
    "wait",
    "snapshot",
    "getbyrole",
    "getbytext",
    "getbylabel",
    "getbyplaceholder",
    "getbyalttext",
    "getbytitle",
    "getbytestid",
    "keyboard",
    "inserttext",
    "back",
    "forward",
    "reload",
    "tab_new",
    "tab_switch",
];

const SCRIPT_KEYS: &[&str] = &["script", "expression", "function", "fn", "js", "code"];

const URL_KEYS: &[&str] = &[
    "url", "href", "src", "action", "uri", "location", "origin", "target",
];

/// Check one value the way a browser reads it: the URL parser drops tabs,
/// newlines and leading control characters, so a string that parses as an
/// http(s) URL is host-checked whatever it looks like before parsing.
fn check_value(
    flow: &Flow,
    v: &Value,
    key: Option<&str>,
    scripts: &mut bool,
) -> Result<(), String> {
    match v {
        Value::String(text) => {
            let forced = key.is_some_and(|k| URL_KEYS.contains(&k));
            if let Ok(parsed) = url::Url::parse(text) {
                match parsed.scheme() {
                    "http" | "https" => check_allowed_host(flow, text)?,
                    "about" if parsed.as_str() == "about:blank" => {}
                    scheme
                        if forced
                            || matches!(
                                scheme,
                                "ws" | "wss" | "ftp" | "file" | "javascript" | "data" | "blob"
                            ) =>
                    {
                        return Err(format!("'{}' is not an http(s) URL", text.trim()));
                    }
                    _ => {}
                }
            }
            Ok(())
        }
        Value::Array(items) => items
            .iter()
            .try_for_each(|i| check_value(flow, i, key, scripts)),
        Value::Object(map) => {
            for (k, item) in map {
                if SCRIPT_KEYS.contains(&k.as_str()) && item.is_string() {
                    *scripts = true;
                }
                check_value(flow, item, Some(k), scripts)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Check that every command a replay would send is a plain interaction and
/// every URL in it stays on the flow's site.
pub fn check_replay_commands(flow: &Flow, commands: &[Value]) -> Result<(), String> {
    for cmd in commands {
        let action = cmd["action"].as_str().unwrap_or("");
        if !REPLAY_ACTIONS.contains(&action) {
            return Err(format!(
                "the recorded command '{}' is not allowed in a replay",
                action
            ));
        }
        let mut scripts = false;
        check_value(flow, cmd, None, &mut scripts)?;
        if scripts {
            return Err(format!(
                "the recorded command '{}' carries a script, which a replay does not run",
                action
            ));
        }
    }
    Ok(())
}

/// The entry captured during a replay that stands for a recorded request:
/// same method, host and path, the same recorded query, and for requests with
/// a body the same body or GraphQL operation (one endpoint serves every
/// query). Query parameters the replay added and the recording never had,
/// such as cache-busters, are ignored. An exact URL wins; otherwise exactly
/// one entry must match, so an ambiguous or missing match is an error.
pub fn replay_lookup<'a>(
    entries: &'a [HarEntry],
    req: &PreparedRequest,
) -> Result<&'a HarEntry, String> {
    let key = |u: &str| {
        url::Url::parse(u)
            .ok()
            .map(|u| (u.host_str().unwrap_or("").to_string(), u.path().to_string()))
    };
    let operation = |b: Option<&str>| {
        b.and_then(|b| serde_json::from_str::<Value>(b).ok())
            .and_then(|v| v["operationName"].as_str().map(String::from))
    };
    let want = key(&req.url);
    let want_op = operation(req.body.as_deref());
    let same_body = |e: &HarEntry| match &req.body {
        None => true,
        Some(b) => {
            let same_json = || {
                let (a, c) = (
                    serde_json::from_str::<Value>(b).ok()?,
                    serde_json::from_str::<Value>(e.post_data.as_deref()?).ok()?,
                );
                Some(a == c)
            };
            e.post_data.as_deref() == Some(b.as_str())
                || same_json() == Some(true)
                || (want_op.is_some() && operation(e.post_data.as_deref()) == want_op)
        }
    };
    let candidates: Vec<&HarEntry> = entries
        .iter()
        .rev()
        .filter(|e| e.method == req.method && key(&e.url) == want && same_body(e))
        .collect();
    if let Some(exact) = candidates.iter().find(|e| e.url == req.url) {
        return Ok(exact);
    }
    let wanted_query: Vec<(String, String)> = url::Url::parse(&req.url)
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let has_query = |e: &HarEntry| {
        let pairs: Vec<(String, String)> = url::Url::parse(&e.url)
            .map(|u| {
                u.query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        wanted_query.iter().all(|(k, v)| {
            pairs
                .iter()
                .any(|(pk, pv)| pk == k && (v.contains("{{") || pv == v))
        })
    };
    let matching: Vec<&HarEntry> = candidates.into_iter().filter(|e| has_query(e)).collect();
    match matching.as_slice() {
        [] => Err(format!(
            "no replayed request matches {} {} with its recorded query",
            req.method, req.url
        )),
        [one] => Ok(one),
        _ => Err(format!(
            "{} {} matches several replayed requests, so none is used",
            req.method, req.url
        )),
    }
}

pub fn replay_match<'a>(entries: &'a [HarEntry], req: &PreparedRequest) -> Option<&'a HarEntry> {
    replay_lookup(entries, req).ok()
}

/// A write the replay already sent has no answer to read back: it is not
/// sent again.
pub fn replay_write_unanswered(
    req: &PreparedRequest,
    started: Instant,
    reason: String,
) -> TierResult {
    tier_result(req, Tier::Replay, started, Err(unknown_outcome(reason)))
}

pub fn replay_result(
    req: &PreparedRequest,
    started: Instant,
    entry: Option<&HarEntry>,
) -> TierResult {
    let outcome = match entry {
        None => Err(Failure::new(
            true,
            None,
            "replaying the recorded commands did not send this request".to_string(),
        )),
        Some(e) => match (e.status, e.response_body.as_deref()) {
            (Some(status), Some(body)) if !e.response_body_base64 => {
                Ok((status as u16, body.to_string()))
            }
            (Some(status), _) => Err(Failure::new(
                true,
                Some(status as u16),
                "the replayed request has no readable body".to_string(),
            )),
            _ => Err(Failure::new(
                true,
                None,
                "the replayed request got no response".to_string(),
            )),
        },
    };
    let retry_after = entry.and_then(|e| {
        e.response_headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("retry-after"))
            .map(|(_, v)| v.clone())
    });
    with_retry_after(
        tier_result(req, Tier::Replay, started, outcome),
        retry_after,
    )
}

pub fn replay_failed(req: &PreparedRequest, started: Instant, reason: String) -> TierResult {
    tier_result(
        req,
        Tier::Replay,
        started,
        Err(Failure::new(true, None, reason)),
    )
}

/// The replay tier cannot express this run; that says nothing about the site.
pub fn replay_unavailable(req: &PreparedRequest, started: Instant, reason: String) -> TierResult {
    tier_result(
        req,
        Tier::Replay,
        started,
        Err(Failure::new(false, None, reason)),
    )
}

/// A reference the run could not fill: the earlier response changed.
pub fn unresolved(req: &PreparedRequest, reason: String) -> TierResult {
    tier_result(
        req,
        Tier::Http,
        Instant::now(),
        Err(Failure::new(true, None, reason)),
    )
}

/// The last failed attempt, when every way of sending that request was
/// refused with a 401 or 403.
pub fn denied_everywhere(results: &[TierResult]) -> Option<&TierResult> {
    let last = results.last().filter(|r| !r.ok)?;
    let attempts: Vec<&TierResult> = results.iter().filter(|r| r.id == last.id).collect();
    (attempts.iter().all(|r| !r.ok) && attempts.iter().any(|r| matches!(r.status, Some(401 | 403))))
        .then_some(last)
}

/// Tiers to try for a request, starting from the one that last worked.
pub fn tiers_for(flow: &Flow, id: &str) -> Vec<Tier> {
    let preferred = flow
        .request(id)
        .and_then(|r| r.tier)
        .and_then(|t| Tier::ALL.iter().position(|x| *x == t))
        .unwrap_or(0);
    Tier::ALL[preferred..].to_vec()
}

/// The direct-HTTP client. Redirects are followed only to hosts the
/// session's domain filter allows, the same rule browser requests follow.
pub fn http_client(
    filter: Option<super::network::DomainFilter>,
    flow: &Flow,
) -> Result<reqwest::Client, String> {
    let flow = flow.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let blocked = check_allowed_host(&flow, attempt.url().as_str())
            .err()
            .or_else(|| {
                filter
                    .as_ref()
                    .and_then(|f| f.check_url(attempt.url().as_str()).err())
            });
        match blocked {
            // The 3xx comes back as the response, so a write that was sent
            // counts as answered.
            Some(_) => attempt.stop(),
            // reqwest's default redirect limit.
            None if attempt.previous().len() >= 10 => attempt.stop(),
            None => attempt.follow(),
        }
    });
    reqwest::Client::builder()
        .redirect(policy)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

pub fn cookie_header(cookies: &[super::cookies::Cookie]) -> String {
    cookies
        .iter()
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fold tier results into the run response. `attempts` holds every tier
/// tried, in order; the last attempt per request is the one that counts.
/// The flow is verified only when every request came back with the recorded
/// shape and no tier saw that shape change.
pub fn finish_run(
    flow: &mut Flow,
    attempts: &[TierResult],
    now: &str,
    total_ms: u128,
    shape: &Shape,
) -> Value {
    let mut last: Vec<&TierResult> = Vec::new();
    for a in attempts {
        match last.iter().position(|r| r.id == a.id) {
            Some(i) => last[i] = a,
            None => last.push(a),
        }
    }
    let all_ok = !last.is_empty() && last.iter().all(|r| r.ok);
    let shape_changed = attempts.iter().find(|a| a.shape_changed);
    // An empty answer cannot confirm the recorded lists still exist, so it
    // leaves the verified state as it was.
    let empty = last.iter().any(|r| r.ok && r.empty);
    if all_ok && shape_changed.is_none() && empty {
    } else if all_ok && shape_changed.is_none() {
        flow.verified_at = Some(now.to_string());
    } else if shape_changed.is_some() || (!all_ok && attempts.iter().any(|r| !r.ok && r.drift)) {
        flow.verified_at = None;
    }
    // Remember the tier that worked, so the next run starts there.
    for r in last.iter().filter(|r| r.ok) {
        for req in flow
            .steps
            .iter_mut()
            .flat_map(|s| s.requests.iter_mut())
            .filter(|req| req.id == r.id)
        {
            req.tier = (r.tier != Tier::Http).then_some(r.tier);
        }
    }
    let output = last
        .iter()
        .find(|r| r.id == flow.output && r.ok)
        .and_then(|r| r.body.clone())
        .map(|b| {
            if empty {
                json!({ "total": 0, "matched": 0, "items": [] })
            } else {
                apply_shape(b, shape)
            }
        });
    let steps: Vec<Value> = last
        .iter()
        .map(|r| {
            let fell_back: Vec<Value> = attempts
                .iter()
                .filter(|a| a.id == r.id && !std::ptr::eq(*a, *r))
                .map(|a| json!({ "tier": a.tier, "error": a.reason }))
                .collect();
            let mut step = json!({
                "id": r.id,
                "step": r.step,
                "tier": if r.ok { json!(r.tier) } else { Value::Null },
                "status": r.status,
                "ms": r.ms,
                "error": r.reason,
            });
            if !fell_back.is_empty() {
                step["fellBack"] = json!(fell_back);
            }
            step
        })
        .collect();
    let mut response = json!({
        "name": flow.name,
        "origin": flow.origin,
        "ok": all_ok,
        "verified": all_ok && shape_changed.is_none() && !empty,
        "ms": total_ms,
        "steps": steps,
        "data": output,
    });
    if empty {
        response["empty"] = json!(true);
    }
    if let Some(a) = shape_changed {
        response["drift"] = json!({ "tier": a.tier, "step": a.step, "reason": a.reason });
    }
    response
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
        r.tag_request("doc");
        r.tag_request("api");
        r.tag_request("ga");
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
        }
    }

    fn attempt(id: &str, tier: Tier, outcome: Result<(u16, String), Failure>) -> TierResult {
        tier_result(&prepared(id), tier, Instant::now(), outcome)
    }

    #[test]
    fn check_response_separates_blocks_inputs_and_shape_changes() {
        let req = prepared("s1r1");
        let blocked = check_response(&req, 403, "").unwrap_err();
        assert!(!blocked.drift && !blocked.shape_changed);
        assert_eq!(blocked.kind, Some("auth_expired"));
        let rejected = check_response(&req, 400, "{}").unwrap_err();
        assert!(!rejected.drift);
        let moved = check_response(&req, 200, r#"{"items":[]}"#).unwrap_err();
        assert!(moved.drift && moved.shape_changed);
        assert!(check_response(&req, 200, r#"{"data":[{"id":2}]}"#).is_ok());
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
            r.tag_request(id);
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
    fn graphql_queries_are_reads_and_mutations_are_writes() {
        assert!(!is_write("GET", None));
        assert!(is_write("POST", Some(r#"{"a":1}"#)));
        assert!(!is_write(
            "POST",
            Some(r#"{"query":"query appConfig { x }"}"#)
        ));
        assert!(!is_write("POST", Some(r#"[{"query":"{ me { id } }"}]"#)));
        assert!(is_write(
            "POST",
            Some(r#"{"query":"mutation buy { order }"}"#)
        ));
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
        assert_eq!(replay_match(&entries, &req).unwrap().request_id, "b");
        req.body = Some(r#"{"operationName":"other"}"#.to_string());
        assert!(replay_match(&entries, &req).is_none());
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
        r.tag_request("api");
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
    fn a_response_without_its_lists_is_empty_not_drift() {
        let req = PreparedRequest {
            expect: expect_of(
                json!({ "signature": signature(&json!({ "payload": { "results": [{ "id": 1 }], "total": 1 } })) }),
            ),
            ..prepared("s1r1")
        };
        let (_, empty) = check_response(&req, 200, r#"{"payload":{}}"#).unwrap();
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
            Ok((200, r#"{"payload":{"total":0}}"#.to_string())),
        )];
        let out = finish_run(&mut flow, &attempts, "now", 1, &Shape::default());
        assert_eq!(out["ok"], true);
        assert_eq!(out["empty"], true);
        assert_eq!(out["verified"], false);
        assert_eq!(flow.verified_at.as_deref(), Some("before"));
    }

    #[test]
    fn params_with_quotes_keep_json_bodies_and_commands_valid() {
        let flow = flow_of(json!({
            "name": "f",
            "params": [{ "name": "q", "example": "kettle" }],
            "output": "s1r1",
            "steps": [
                { "command": { "action": "fill", "selector": "#q", "value": "kettle" },
                  "requests": [{ "id": "s1r1", "method": "POST", "url": "https://site.example/api",
                    "headers": {}, "body": "{\"query\":\"{{q}}\"}", "expect": {} }] },
            ],
        }));
        let value = "a \"quoted\" item\\";
        let reqs = prepare(&flow, &[("q".to_string(), value.to_string())]).unwrap();
        let body: Value = serde_json::from_str(reqs[0].body.as_deref().unwrap()).unwrap();
        assert_eq!(body["query"], value);
        let cmds = replay_commands(&flow, &[("q".to_string(), value.to_string())]).unwrap();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[1]["value"], value);
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
        r.tag_request("buy");
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
            replay_only_writes(&flow_of(flow.clone())).len() == 1
                && request_write_ids(&flow_of(flow.clone())).is_empty()
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
        r.tag_request("login");
        r.tag_request("me");
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

    fn recorded(steps: &[(&str, bool)]) -> FlowRecorder {
        let mut r = FlowRecorder::default();
        r.begin_capture(None);
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
        assert_eq!(r.resolve_range(Some("open"), None).unwrap(), (3, 4));
        assert_eq!(r.resolve_range(Some("2"), Some(3)).unwrap(), (2, 3));
        r.last_save_seq = 4;
        assert_eq!(r.unsaved_steps(), 0);
        assert!(r.resolve_range(None, None).is_err());
        let seq = r
            .begin_command("click", &json!({ "action": "click" }), None, None)
            .unwrap();
        r.finish_command(seq, true, None);
        assert_eq!(r.resolve_range(None, None).unwrap(), (5, 5));
        assert_eq!(r.resolve_range(Some("last-save"), None).unwrap(), (5, 5));
        assert_eq!(r.unsaved_steps(), 1);
        assert!(r.resolve_range(Some("x"), None).is_err());
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
        r.tag_request("late");
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
    fn hosts_outside_the_start_domain_are_rejected() {
        let flow = flow_of(
            json!({ "startUrl": "https://www.example.com/shop", "allowedHosts": ["example.com"] }),
        );
        assert!(check_allowed_host(&flow, "https://api.example.com/x").is_ok());
        assert!(check_allowed_host(&flow, "https://example.com/x").is_ok());
        for bad in [
            "https://evil.com/x",
            "https://example.com.evil.com/x",
            "https://notexample.com/x",
            "ftp://example.com/x",
            "not a url",
        ] {
            assert!(check_allowed_host(&flow, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_edited_allowed_hosts_list_cannot_widen_the_start_domain() {
        let flow = flow_of(
            json!({ "startUrl": "https://example.com/", "allowedHosts": ["example.com", "evil.com"] }),
        );
        assert!(check_allowed_host(&flow, "https://evil.com/x").is_err());
        let narrow = flow_of(
            json!({ "startUrl": "https://example.com/", "allowedHosts": ["api.example.com"] }),
        );
        assert!(check_allowed_host(&narrow, "https://www.example.com/x").is_err());
        assert!(check_allowed_host(&narrow, "https://api.example.com/x").is_ok());
    }

    #[test]
    fn ip_hosts_match_exactly() {
        let flow =
            flow_of(json!({ "startUrl": "http://127.0.0.1:3000/", "allowedHosts": ["127.0.0.1"] }));
        assert!(check_allowed_host(&flow, "http://127.0.0.1:3000/api").is_ok());
        assert!(check_allowed_host(&flow, "http://10.0.0.1/api").is_err());
        assert_eq!(base_domain("127.0.0.1"), "127.0.0.1");
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
        let flow = flow_of(json!({ "version": 2, "name": "x", "origin": "../escape" }));
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
            r.tag_request(t);
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
    fn a_get_a_click_fired_that_no_output_uses_is_a_suspected_write() {
        let list = entry(
            "list",
            "https://api.example.com.ar/items",
            "Fetch",
            "application/json",
            r#"{"items":[{"id":1}]}"#,
        );
        let add = entry(
            "add",
            "https://www.example.com.ar/cart/add?id=7",
            "Fetch",
            "application/json",
            r#"{"added":true}"#,
        );
        let writes = writes_after("click", vec![list.clone(), add], &["list", "add"]);
        let writes = writes.as_array().unwrap();
        assert_eq!(writes.len(), 1, "{writes:?}");
        assert_eq!(writes[0]["kind"], "suspected");
        assert_eq!(writes[0]["method"], "GET");
        assert!(writes[0]["url"]
            .as_str()
            .unwrap()
            .contains("/cart/add?id=7"));
        let only_list = writes_after("click", vec![list], &["list"]);
        assert_eq!(only_list, json!([]));
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
            r.tag_request(t);
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
        assert!(check_write_ids(&flow, &some).unwrap_err().contains("s3w1"));
    }

    fn traits(input_type: &str, autocomplete: &str, hint: &str) -> FieldTraits {
        FieldTraits {
            input_type: input_type.to_string(),
            autocomplete: autocomplete.to_string(),
            hint: hint.to_string(),
        }
    }

    #[test]
    fn a_field_is_secret_by_type_or_autocomplete_even_without_a_label() {
        for t in [
            traits("password", "", ""),
            traits("text", "one-time-code", ""),
            traits("text", "current-password", ""),
            traits("text", "new-password", ""),
            traits("text", "cc-number", ""),
            traits("tel", "section-pay cc-csc", ""),
        ] {
            let mut r = recorded(&[]);
            r.begin_command(
                "fill",
                &json!({ "action": "fill", "selector": "@e5", "value": "482913" }),
                None,
                Some(&t),
            );
            assert_eq!(r.commands[0].1["value"], REDACTED, "{t:?}");
            assert_eq!(r.commands[0].1["redacted"], true);
            assert_eq!(r.secrets, vec!["482913".to_string()]);
            assert!(r.typed.is_empty());
        }
        let mut r = recorded(&[]);
        r.begin_command(
            "fill",
            &json!({ "action": "fill", "selector": "@e5", "value": "x" }),
            None,
            Some(&traits("text", "email", "email")),
        );
        assert_ne!(r.commands[0].1["value"], REDACTED);
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
        r.tag_request("lookup");
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
        r.tag_request("search");
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
    fn statuses_that_end_a_run_carry_a_kind_and_no_drift() {
        let req = prepared("s1r1");
        for (status, kind) in [
            (401, "auth_expired"),
            (403, "auth_expired"),
            (429, "rate_limited"),
        ] {
            let f = check_response(&req, status, "{}").unwrap_err();
            assert_eq!(f.kind, Some(kind), "{status}");
            assert!(!f.drift);
        }
        let blocked = check_response(&req, 200, "<html>captcha</html>").unwrap_err();
        assert_eq!(blocked.kind, Some("blocked"));
        let blocked_403 = check_response(&req, 403, "<div id=cf-chl>").unwrap_err();
        assert_eq!(blocked_403.kind, Some("blocked"));
        assert_eq!(check_response(&req, 500, "{}").unwrap_err().kind, None);
        assert_eq!(check_response(&req, 407, "{}").unwrap_err().kind, None);
    }

    #[test]
    fn retry_after_is_kept_only_for_rate_limits() {
        let req = prepared("s1r1");
        let limited = tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Err(Failure::stop("rate_limited", 429, "x".into())),
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
            Err(Failure::stop("auth_expired", 401, "x".into())),
        );
        assert_eq!(with_retry_after(auth, Some("30".into())).retry_after, None);
    }

    #[test]
    fn a_run_gates_only_the_writes_its_output_needs() {
        let request = |id: &str, url: &str, body: &str| json!({ "id": id, "method": "POST", "url": url, "body": body, "write": true });
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
        assert_eq!(request_write_ids(&flow), vec!["s1r1", "s2r1"]);
        let only_output = flow_of(json!({
            "output": "s2r1", "writes": writes,
            "steps": [
                { "command": {}, "requests": [request("s1r1", "https://a.test/session", "{}")] },
                { "command": {}, "requests": [request("s2r1", "https://a.test/cart", "{}")] }
            ]
        }));
        assert_eq!(request_write_ids(&only_output), vec!["s2r1"]);
        let replay: Vec<String> = replay_only_writes(&only_output)
            .into_iter()
            .map(|w| w.id)
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
            Err(Failure::stop("auth_expired", status, "x".into())),
        )
    }

    #[test]
    fn a_401_on_a_request_recorded_with_credentials_falls_through() {
        let http = denied("s1r1", Tier::Http, 401);
        assert_eq!(http.kind, None);
        assert!(http.drift);
        let page = denied("s1r1", Tier::Page, 403);
        assert_eq!(page.kind, None);
        let replay = denied("s1r1", Tier::Replay, 401);
        assert_eq!(replay.kind, Some("auth_expired"));
        // Without stripped credentials the first 401 still stops the run.
        let req = prepared("s1r1");
        let plain = tier_result(
            &req,
            Tier::Http,
            Instant::now(),
            Err(Failure::stop("auth_expired", 401, "x".into())),
        );
        assert_eq!(plain.kind, Some("auth_expired"));
        // When no way gets through, the run reports the sign-in problem.
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
        r.tag_request("api");
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
    fn json_that_mentions_a_captcha_is_data_not_a_challenge() {
        let req = prepared("s1r1");
        assert!(
            check_response(&req, 200, r#"{"data":[{"id":1,"note":"captcha solved"}]}"#).is_ok()
        );
        let html = check_response(&req, 200, "<html><body>captcha</body></html>").unwrap_err();
        assert_eq!(html.kind, Some("blocked"));
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
                check_replay_commands(&flow, &[bad.clone()]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn public_suffixes_keep_sibling_sites_apart() {
        assert_eq!(base_domain("a.vercel.app"), "a.vercel.app");
        assert_ne!(base_domain("a.vercel.app"), base_domain("b.vercel.app"));
        assert_eq!(base_domain("api.example.com.ar"), "example.com.ar");
        assert_eq!(base_domain("localhost"), "localhost");
        let flow = flow_of(
            json!({ "startUrl": "https://a.vercel.app/", "allowedHosts": ["a.vercel.app"] }),
        );
        assert!(check_allowed_host(&flow, "https://api.a.vercel.app/x").is_ok());
        assert!(check_allowed_host(&flow, "https://b.vercel.app/x").is_err());
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
            assert_eq!(r.secrets, vec!["42".to_string()]);
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
        r.tag_request("s");
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
        assert!(r.typed.is_empty());
        assert_eq!(r.secrets, vec!["s3cret!".to_string()]);
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
        r.tag_request("belongs-to-step-1");
        r.untag_current();
        r.tag_request("from-an-unrecorded-command");
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
        assert_eq!(r.secrets, vec!["pass".to_string()]);
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
        assert_eq!(r.secrets, vec!["pass".to_string(), "z".to_string()]);
        // Keys in an ordinary field stay as they are.
        let mut plain = recorded(&[]);
        plain.begin_command(
            "press",
            &json!({ "action": "press", "key": "q" }),
            None,
            Some(&traits("text", "", "search")),
        );
        assert_eq!(plain.commands[0].1["key"], "q");
        assert!(plain.secrets.is_empty());
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
        assert_eq!(r.secrets, vec!["A".to_string()]);
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
        r.tag_request("login");
        r.tag_request("me");
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
        assert_eq!(r.secrets, vec!["hunter2".to_string()]);
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
        assert_eq!(r.secrets, vec!["x".to_string()]);
        // Shift is a plain character modifier.
        let mut r = recorded(&[]);
        r.begin_command(
            "press",
            &json!({ "action": "press", "key": "Shift+H" }),
            None,
            Some(&secret),
        );
        assert_eq!(r.secrets, vec!["H".to_string()]);
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
        r.tag_request("login");
        r.tag_request("me");
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
        r.tag_request("home");
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
        assert_eq!(r.secrets, vec!["s3cret-token".to_string()]);
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
        assert!(r.secrets.contains(&"pasted-value".to_string()));
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
        r.tag_request("login");
        r.tag_request("me");
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
        assert_eq!(r.secrets.last().unwrap(), "apasted!");
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
    fn comment_3_params_with_quotes_keep_commands_and_bodies_valid() {
        let value = r#"a "quoted" item"#.to_string();
        let flow = flow_of(json!({
            "startUrl": "https://a.test/",
            "params": [{ "name": "q", "source": "typed" }],
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
    fn comment_4_type_into_a_password_selector_is_redacted() {
        let mut r = recorded(&[]);
        r.begin_command(
            "type",
            &json!({ "action": "type", "selector": "#password", "text": "secret" }),
            None,
            None,
        );
        assert_eq!(r.commands[0].1["text"], REDACTED);
        assert_eq!(r.secrets, vec!["secret".to_string()]);
    }

    #[test]
    fn comment_5_a_json_login_that_carries_a_filled_password_is_not_saved() {
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
        r.tag_request("login");
        r.tag_request("me");
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
    fn comment_6_form_and_query_encodings_of_a_secret_are_recognized() {
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
    fn comment_7_a_write_takes_a_value_from_an_earlier_response_and_depends_on_it() {
        let mut r = recorded(&[]);
        r.begin_command(
            "navigate",
            &json!({ "action": "navigate", "url": "https://www.example.com.ar/" }),
            None,
            None,
        );
        r.tag_request("session");
        r.begin_command(
            "click",
            &json!({ "action": "click", "selector": "#add" }),
            None,
            None,
        );
        r.tag_request("cart");
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
        assert!(replay_match(&entries, &want("https://api.site.example/items?page=1")).is_some());
        assert!(replay_match(&entries, &want("https://api.site.example/items")).is_none());
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
}
