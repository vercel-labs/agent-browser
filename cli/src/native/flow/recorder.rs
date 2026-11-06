//! The recorder: what a `--capture flow` session keeps of the commands it runs.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::{
    build_flow, carries_secret, classify_header, describe_command, dropped_name,
    may_become_candidate, save_start_url, write_flow, HeaderClass, HostPolicy, SaveInput,
};
use crate::native::actions::{HarEntry, FIND_ACTIONS};
use crate::native::element::RefMap;

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

/// What a save needs besides the recording.
pub struct SaveArgs<'a> {
    pub name: &'a str,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub description: Option<&'a str>,
    pub params: &'a [(String, String)],
    pub output: Option<&'a str>,
    pub entries: &'a [HarEntry],
    /// The page shows a password field at save time.
    pub login_wall: bool,
    /// See `SaveInput::local_names`.
    pub local_names: &'a [(String, super::net::Reach)],
}

/// What the recorder knows about a command besides its arguments.
#[derive(Clone, Default)]
pub struct StepMeta {
    pub(super) ok: bool,
    /// Page URL before the command ran.
    pub(super) from_url: Option<String>,
    /// Page URL after the command finished.
    pub(super) url: Option<String>,
}

/// Commands and request tags recorded while the session runs with
/// `--capture flow`.
#[derive(Default)]
pub struct FlowRecorder {
    capturing: bool,
    suspended: bool,
    pub(super) commands: Vec<(u64, Value)>,
    pub(super) meta: HashMap<u64, StepMeta>,
    /// HAR request id to the sequence number of the command that was
    /// running when the request started.
    pub(super) request_tags: HashMap<String, u64>,
    /// When each command began (epoch seconds) and the step it recorded, or
    /// none for a command that is not recorded. A request belongs to the last
    /// window that began before it started.
    windows: Vec<(f64, Option<u64>)>,
    /// A paste whose value is read once the command has finished.
    pub(super) paste_pending: Option<u64>,
    /// Steps that put a value of unknown content into a secret field.
    pub(super) unknown_secret_seqs: Vec<u64>,
    /// Keys pressed so far in the secret field that has focus.
    key_buffer: String,
    /// The last entry of `secrets` is the current `key_buffer`.
    key_active: bool,
    /// A typed step saved as a param before its field could be read.
    pub(super) needs_traits: Option<u64>,
    /// Sequence number the command being executed was recorded under.
    pub(super) begun: Option<u64>,
    /// Values typed into secret fields, kept in memory only, so save can
    /// drop any captured request that carries one.
    pub(super) secrets: Vec<String>,
    /// The tab's URL when capture began, for flows that start on an open page.
    pub(super) page_url: Option<String>,
    /// Values typed into fields that are not secrets, as (param name, value, seq).
    pub(super) typed: Vec<(String, String, u64)>,
    /// Sequence number of the last step covered by a save.
    pub(super) last_save_seq: u64,
    /// Flows saved during this session, as (origin, name).
    pub(super) saved: Vec<(String, String)>,
    next_seq: u64,
}

impl FlowRecorder {
    pub fn active(&self) -> bool {
        self.capturing
    }

    // The interface the daemon uses: begin, tag, end, steps, save.

    /// A command starts: take over from the command that is running, if any.
    pub fn enter(&mut self) -> Option<u64> {
        self.begun.take()
    }

    /// The command that just finished, and hand back to the outer one.
    pub fn exit(&mut self, outer: Option<u64>) -> Option<u64> {
        std::mem::replace(&mut self.begun, outer)
    }

    /// Whether a command would be recorded, so the caller reads the page
    /// only when the recorder will use it.
    pub fn wants_context(&self, action: &str) -> bool {
        self.records(action)
    }

    /// Record a command and return its step number, or note that a command
    /// that is not recorded has started so its requests belong to no step.
    pub fn begin(
        &mut self,
        action: &str,
        cmd: &Value,
        refs: &RefMap,
        from_url: Option<String>,
        traits: Option<&FieldTraits>,
    ) -> Option<u64> {
        if self.records(action) {
            let stable = stable_command(action, cmd, refs);
            self.begin_command(action, &stable, from_url, traits)
        } else {
            if self.active() {
                self.untag_current();
            }
            None
        }
    }

    /// What the page must be asked after a command: the focused field's
    /// value (a paste) and its traits (a typed step whose field was unknown).
    pub fn after_reads(&self, seq: u64) -> (bool, bool) {
        (
            self.paste_pending == Some(seq),
            self.needs_traits == Some(seq),
        )
    }

    /// A command finished.
    pub fn end(
        &mut self,
        seq: u64,
        ok: bool,
        url: Option<String>,
        field_value: Option<String>,
        field_traits: Option<FieldTraits>,
    ) {
        if self.paste_pending == Some(seq) {
            self.resolve_paste(seq, field_value);
        }
        if self.needs_traits == Some(seq) {
            if let Some(traits) = field_traits {
                self.redact_if_secret(seq, &traits);
            }
        }
        self.finish_command(seq, ok, url);
    }

    /// The recorded steps, as `flow steps` shows them.
    pub fn steps(&self) -> Value {
        let steps: Vec<Value> = self
            .commands
            .iter()
            .map(|(seq, c)| {
                json!({
                    "seq": seq,
                    "command": super::describe_command(c),
                    "url": self.meta.get(seq).and_then(|m| m.url.clone()),
                    "requests": self.request_tags.values().filter(|s| *s == seq).count(),
                    "ok": self.is_ok(*seq),
                    "saved": *seq <= self.last_save_seq,
                })
            })
            .collect();
        json!({
            "steps": steps,
            "lastSave": self.last_save_seq,
            "unsaved": self.unsaved_steps(),
        })
    }

    /// Flows saved during this session, as (origin, name).
    pub fn saved(&self) -> &[(String, String)] {
        &self.saved
    }

    /// Save a range of the recording as a flow and write it. Capture goes on.
    pub fn save(&mut self, args: SaveArgs<'_>) -> Result<Value, String> {
        let (first, last) = self.resolve_range(args.from, args.to)?;
        let commands: Vec<(u64, Value)> = self
            .commands
            .iter()
            .filter(|(seq, _)| *seq >= first && *seq <= last)
            .cloned()
            .collect();
        let (flow, mut report) = build_flow(SaveInput {
            name: args.name,
            recorder: self,
            commands: &commands,
            description: args.description,
            entries: args.entries,
            params: args.params,
            output: args.output,
            created_at: chrono::Utc::now().to_rfc3339(),
            login_wall: args.login_wall,
            local_names: args.local_names,
        })?;
        let path = write_flow(&flow)?;
        self.mark_saved(last);
        self.saved
            .push((flow.origin.clone(), args.name.to_string()));
        report["path"] = json!(path.to_string_lossy());
        report["from"] = json!(first);
        report["to"] = json!(last);
        Ok(report)
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

    pub(super) fn unsaved_steps(&self) -> usize {
        self.commands
            .iter()
            .filter(|(seq, _)| *seq > self.last_save_seq)
            .count()
    }

    /// Commands run by a replay are not part of the recording.
    pub fn set_suspended(&mut self, suspended: bool) {
        if suspended && !self.suspended {
            self.windows.push((now(), None));
        }
        self.suspended = suspended;
    }

    /// Record a command and return its sequence number. The step counts as
    /// failed until `finish_command` says otherwise.
    pub fn records(&self, action: &str) -> bool {
        self.active() && !self.suspended && is_recordable(action)
    }

    pub(super) fn begin_command(
        &mut self,
        action: &str,
        cmd: &Value,
        from_url: Option<String>,
        traits: Option<&FieldTraits>,
    ) -> Option<u64> {
        self.begin_command_at(now(), action, cmd, from_url, traits)
    }

    pub(super) fn begin_command_at(
        &mut self,
        at: f64,
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
        self.secrets.extend(redact_headers(&mut recorded));
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
        self.windows.push((at, Some(self.next_seq)));
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
    pub(super) fn redact_if_secret(&mut self, seq: u64, traits: &FieldTraits) {
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

    pub(super) fn finish_command(&mut self, seq: u64, ok: bool, url: Option<String>) {
        if let Some(m) = self.meta.get_mut(&seq) {
            m.ok = ok;
            m.url = url;
        }
    }

    /// A request started at `started_at` (epoch seconds). It belongs to the
    /// command that was running then, not the one running when the event
    /// was read, so a response that lands late keeps its step.
    pub fn tag(&mut self, request_id: &str, started_at: f64) {
        let started_at = if started_at > 0.0 { started_at } else { now() };
        let seq = self
            .windows
            .iter()
            .rev()
            .find(|(begin, _)| *begin <= started_at)
            .and_then(|(_, seq)| *seq);
        if let Some(seq) = seq {
            self.request_tags.insert(request_id.to_string(), seq);
        }
    }

    #[cfg(test)]
    pub(super) fn tag_now(&mut self, request_id: &str) {
        self.tag(request_id, now());
    }

    /// The start host of a save, when named by a domain, its port, and the
    /// addresses the browser reached it at in the steps the save covers.
    pub fn start_host_addresses(
        &self,
        from: Option<u64>,
        to: Option<u64>,
        entries: &[HarEntry],
    ) -> Option<(String, u16, Vec<std::net::SocketAddr>)> {
        let (first, last) = self.resolve_range(from, to).ok()?;
        let commands: Vec<(u64, Value)> = self
            .commands
            .iter()
            .filter(|(seq, _)| *seq >= first && *seq <= last)
            .cloned()
            .collect();
        let start = super::save_start_url(&commands, self)?;
        let start = url::Url::parse(&start).ok()?;
        let Some(url::Host::Domain(host)) = start.host() else {
            return None;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let port = start.port_or_known_default()?;
        let addrs = entries
            .iter()
            .filter(|e| {
                self.request_tags
                    .get(&e.request_id)
                    .is_some_and(|seq| *seq >= first && *seq <= last)
            })
            .filter(|e| {
                url::Url::parse(&e.url).ok().is_some_and(|u| {
                    u.host_str()
                        .is_some_and(|h| h.trim_end_matches('.').eq_ignore_ascii_case(&host))
                        && u.port_or_known_default() == Some(port)
                })
            })
            .filter_map(|e| e.remote_addr)
            .collect();
        Some((host, port, addrs))
    }

    /// The first and last step a save covers. Without `from` the save
    /// starts at the last navigation after the previous save.
    pub(super) fn resolve_range(
        &self,
        from: Option<u64>,
        to: Option<u64>,
    ) -> Result<(u64, u64), String> {
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
        let from = from.unwrap_or_else(|| {
            self.commands
                .iter()
                .rev()
                .filter(|(seq, c)| {
                    *seq <= to
                        && *seq > self.last_save_seq
                        && c.get("action").and_then(|a| a.as_str()) == Some("navigate")
                })
                .map(|(seq, _)| *seq)
                .next()
                .unwrap_or(self.last_save_seq + 1)
        });
        if from == 0 || from > to {
            return Err(format!(
                "no recorded steps from {} to {}; run `flow steps` to see them",
                from, to
            ));
        }
        Ok((from, to))
    }

    /// A gap for each request that could become a flow request, tagged to a
    /// step in the range, that has not finished loading, so its response body
    /// is not on the entry yet. Third-party, telemetry and streaming requests
    /// never count. The gap names the step that started the request and the
    /// request by method and path, with a secret hidden as a dropped request is.
    pub fn loading_gaps(
        &self,
        from: Option<u64>,
        to: Option<u64>,
        entries: &[HarEntry],
    ) -> Vec<Value> {
        self.gaps_of_loading(from, to, entries, None)
    }

    /// Whether a request of the range is still loading and worth waiting
    /// on: one the last step started that has no response yet, or one whose
    /// body is still arriving (not `stalled`).
    pub fn awaiting_response(
        &self,
        from: Option<u64>,
        to: Option<u64>,
        entries: &[HarEntry],
        stalled: &dyn Fn(&HarEntry) -> bool,
    ) -> bool {
        !self
            .gaps_of_loading(from, to, entries, Some(stalled))
            .is_empty()
    }

    fn gaps_of_loading(
        &self,
        from: Option<u64>,
        to: Option<u64>,
        entries: &[HarEntry],
        stalled: Option<&dyn Fn(&HarEntry) -> bool>,
    ) -> Vec<Value> {
        let Ok((first, last)) = self.resolve_range(from, to) else {
            return Vec::new();
        };
        let commands: Vec<(u64, Value)> = self
            .commands
            .iter()
            .filter(|(seq, _)| *seq >= first && *seq <= last)
            .cloned()
            .collect();
        let Some(start) = save_start_url(&commands, self) else {
            return Vec::new();
        };
        let policy = HostPolicy::from_start(&start);
        entries
            .iter()
            .filter(|e| e.loading_finished_timestamp.is_none() && may_become_candidate(e, &policy))
            .filter(|e| match stalled {
                None => true,
                // A request with no answer yet is waited on only if the last
                // step started it; one from an earlier step outlived the
                // steps after it, as a long poll does.
                Some(_) if e.status.is_none() => {
                    self.request_tags.get(&e.request_id) == Some(&last)
                }
                Some(s) => !s(e),
            })
            .filter_map(|e| {
                let seq = *self.request_tags.get(&e.request_id)?;
                let index = commands.iter().position(|(s, _)| *s == seq)?;
                let known = carries_secret(e, &self.secrets);
                let label = if known || self.unknown_secret_seqs.contains(&seq) {
                    dropped_name(e, &self.secrets, known)
                } else {
                    let path = url::Url::parse(&e.url)
                        .map(|u| u.path().to_string())
                        .unwrap_or_else(|_| e.url.clone());
                    format!("{} {}", e.method, path)
                };
                Some(json!({
                    "step": index + 1,
                    "command": describe_command(&commands[index].1),
                    "reason": format!("still loading at save: {label}"),
                }))
            })
            .collect()
    }

    /// A command that is not recorded starts: requests it sends belong to no
    /// step, so a later save does not keep them.
    pub(super) fn untag_current(&mut self) {
        self.windows.push((now(), None));
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
    pub(super) fn resolve_paste(&mut self, seq: u64, value: Option<String>) {
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

    pub(super) fn mark_saved(&mut self, last: u64) {
        self.last_save_seq = self.last_save_seq.max(last);
    }

    pub(super) fn is_ok(&self, seq: u64) -> bool {
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
    pub(super) fn looks_secret(&self) -> bool {
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

/// Headers a command sends, such as `navigate --headers`, are classified like
/// a request's: a credential or cookie value is never stored. The step is
/// marked redacted, and the values count as secrets.
fn redact_headers(cmd: &mut Value) -> Vec<String> {
    let mut found = Vec::new();
    let Some(headers) = cmd.get_mut("headers").and_then(|h| h.as_object_mut()) else {
        return found;
    };
    for (name, value) in headers.iter_mut() {
        let lower = name.to_ascii_lowercase();
        let secret = classify_header(&lower) == HeaderClass::Credential
            || matches!(lower.as_str(), "cookie" | "set-cookie");
        if secret {
            if let Some(text) = value.as_str().filter(|t| !t.is_empty()) {
                found.push(text.to_string());
            }
            *value = json!(REDACTED);
        }
    }
    if !found.is_empty() || headers.values().any(|v| v == REDACTED) {
        cmd["redacted"] = json!(true);
    }
    found
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
pub(super) fn typed_param_name(traits: Option<&FieldTraits>, taken: &[String]) -> String {
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

/// Snapshot refs only live until the next snapshot, so a recorded command
/// that targets one is stored as the role and name the ref pointed at.
fn stable_command(action: &str, cmd: &Value, refs: &RefMap) -> Value {
    if !FIND_ACTIONS.contains(&action) {
        return cmd.clone();
    }
    let Some(entry) = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .and_then(|s| s.strip_prefix('@'))
        .and_then(|r| refs.get(r))
        .filter(|e| !e.role.is_empty() && !e.name.is_empty())
    else {
        return cmd.clone();
    };
    let mut out = json!({
        "action": "getbyrole",
        "role": entry.role,
        "name": entry.name,
        "exact": true,
        "subaction": action,
    });
    if let Some(v) = cmd.get("value") {
        out["value"] = v.clone();
    }
    out
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
