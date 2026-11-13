//! Planning: what a patch changes in a running session.
//!
//! `Running::plan` is the only place settings are compared. It is pure: the
//! caller applies the plan and then stores `next`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

use super::attach::{reach, Engine, Reach, FIREFOX};
use super::patch::{Patch, Requested};
use super::running::{Connection, Running};
use super::spec::{redacted, spec, Effect, Key, SPECS};

/// One setting whose requested value differs from the running one.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub key: Key,
    pub requested: Requested,
    /// The running value; `None` when the setting was at its default.
    pub running: Option<Value>,
    /// What the change does to this session: apply in place, or need a browser.
    pub effect: Effect,
}

impl Change {
    /// An ignored request as reported to the caller.
    pub fn to_ignored_json(&self) -> Value {
        let mut report = self.to_json(false);
        report["effect"] = json!("ignored");
        report["reason"] = json!(
            "applies only to a browser agent-browser launches itself, not to the browser this session attached to"
        );
        report
    }

    /// The change as reported to the caller. Secrets are redacted. `applied`
    /// is false when no browser was running, so the value only takes effect
    /// at the next launch.
    pub fn to_json(&self, applied: bool) -> Value {
        let s = spec(self.key);
        json!({
            "option": s.name,
            "requested": match &self.requested {
                Requested::Set(v) => redacted(self.key, v),
                Requested::Default => Value::Null,
            },
            "running": self.running.as_ref().map(|v| redacted(self.key, v)),
            "effect": match self.effect {
                Effect::Launch => "relaunch",
                Effect::Live => "live",
            },
            "applied": applied,
        })
    }
}

/// What to do for a patch against a running session.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Changes carried into an attach whose engine is not known yet. The attach keeps them
    /// when it finds an engine that takes them; otherwise they are put back and reported
    /// as ignored.
    pub awaiting_engine: Vec<Change>,
    /// The browser has to be relaunched: a launch setting changed or the
    /// invocation forced it.
    pub relaunch: bool,
    /// The settings after the plan is applied.
    pub next: Running,
    /// Requested values that only describe a browser agent-browser launches
    /// itself, for a session attached to an external browser. They change
    /// nothing and never relaunch.
    pub ignored: Vec<Change>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && !self.relaunch
    }

    /// The part of the plan a command that never launches a browser applies:
    /// its in-place changes, taken from `before` so nothing a launch would carry
    /// moves into the running settings.
    pub fn live_only(&self, before: &Running) -> Plan {
        let mut next = before.clone();
        let mut changes = Vec::new();
        for change in &self.changes {
            if change.effect != Effect::Live {
                continue;
            }
            match &change.requested {
                Requested::Set(value) => next.assign(change.key, value.clone()),
                Requested::Default => next.unassign(change.key),
            }
            changes.push(change.clone());
        }
        Plan {
            changes,
            awaiting_engine: Vec::new(),
            relaunch: false,
            next,
            ignored: self.ignored.clone(),
        }
    }
}

/// The connection target keys; setting one displaces the others.
const CONNECTION_KEYS: [Key; 3] = [Key::Cdp, Key::AutoConnect, Key::Provider];

/// Local-launch settings an external session must not keep: state replayed
/// into the browser and proxy credentials answered on its behalf are the ones
/// that would reach it. The rest of the local-only settings are only ignored.
const DROPPED_FOR_EXTERNAL: [Key; 9] = [
    Key::StorageState,
    Key::Profile,
    Key::Extensions,
    Key::Proxy,
    Key::ProxyBypass,
    Key::ProxyUsername,
    Key::ProxyPassword,
    Key::ExecutablePath,
    Key::Args,
];

/// The value a setting has when nothing sets it, where that is not "absent".
fn default_value(key: Key) -> Option<Value> {
    match key {
        Key::HideScrollbars | Key::Webmcp => Some(Value::Bool(true)),
        Key::Headed
        | Key::IgnoreHttpsErrors
        | Key::AllowFileAccess
        | Key::Webgpu
        | Key::NoXvfb
        | Key::NoAutoDialog
        | Key::PinTab
        | Key::Debug => Some(Value::Bool(false)),
        Key::RestoreSave => Some(Value::from("auto")),
        _ => None,
    }
}

impl Running {
    /// The running value of `key`, or its default.
    fn effective(&self, key: Key) -> Option<Value> {
        self.get(key).cloned().or_else(|| default_value(key))
    }

    /// Plans `patch`. Rules:
    /// - a key the patch does not name never produces a change;
    /// - a value equal to the running one (or to the default when nothing is
    ///   running) is free: no change, no notice, no relaunch;
    /// - a setting an attached browser cannot take is ignored, as `attach` says;
    /// - a setting only an attached Firefox takes is carried into an attach whose
    ///   engine is not known yet, as `Plan::awaiting_engine` records;
    /// - setting one connection target displaces the other targets.
    pub fn plan(&self, patch: &Patch) -> Plan {
        let mut next = self.clone();
        let mut changes: Vec<Change> = Vec::new();
        let mut awaiting_engine: Vec<Change> = Vec::new();
        let mut ignored: Vec<Change> = Vec::new();
        let named_engine = patch.value(Key::Engine).and_then(Value::as_str);

        for (key, requested) in patch.iter() {
            match requested {
                Requested::Set(value) if key == Key::ClearCaCert => {
                    // A one-shot: drops the CA trust that is in effect.
                    if value == &Value::Bool(true) {
                        if let Some(current) = self.get(Key::CaCert) {
                            changes.push(change(
                                key,
                                requested.clone(),
                                Some(current.clone()),
                                Reach::Launch,
                            ));
                            next.remove(Key::CaCert);
                        }
                    }
                }
                Requested::Set(value) => {
                    if self.effective(key).as_ref() == Some(value)
                        && !patch.is_changed_in_place(key)
                    {
                        continue;
                    }
                    let reach = reach_for(&next, self, key, named_engine);
                    if reach == Reach::Ignored {
                        ignored.push(change(
                            key,
                            requested.clone(),
                            self.get(key).cloned(),
                            reach,
                        ));
                        continue;
                    }
                    let recorded = change(key, requested.clone(), self.get(key).cloned(), reach);
                    if reach == Reach::Awaiting {
                        awaiting_engine.push(recorded.clone());
                    }
                    changes.push(recorded);
                    next.assign(key, value.clone());
                    if CONNECTION_KEYS.contains(&key) {
                        displace_other_targets(&mut next, patch, key, &mut changes);
                    }
                    // Headers belong to the endpoint they were set for.
                    if key == Key::Cdp {
                        displace(&mut next, patch, &[Key::CdpHeaders], &mut changes);
                    }
                    // Storage state needs a clean browser, and a profile or
                    // extensions cannot be combined with it. The one set last
                    // wins, so an inherited value cannot wedge the session.
                    match key {
                        Key::Proxy => displace(
                            &mut next,
                            patch,
                            &[Key::ProxyUsername, Key::ProxyPassword],
                            &mut changes,
                        ),
                        Key::StorageState => displace(
                            &mut next,
                            patch,
                            &[Key::Profile, Key::Extensions],
                            &mut changes,
                        ),
                        Key::Profile | Key::Extensions => {
                            displace(&mut next, patch, &[Key::StorageState], &mut changes)
                        }
                        _ => {}
                    }
                }
                Requested::Default => {
                    // Read `next`: a target set earlier in this patch may have
                    // displaced the key already, and that change is recorded.
                    let Some(current) = next.get(key).cloned() else {
                        continue;
                    };
                    let current = &current;
                    if default_value(key).as_ref() == Some(current) {
                        continue;
                    }
                    let reach = reach_for(&next, self, key, named_engine);
                    if reach == Reach::Ignored {
                        ignored.push(change(
                            key,
                            Requested::Default,
                            Some(current.clone()),
                            reach,
                        ));
                        continue;
                    }
                    let recorded = change(key, Requested::Default, Some(current.clone()), reach);
                    if reach == Reach::Awaiting {
                        awaiting_engine.push(recorded.clone());
                    }
                    changes.push(recorded);
                    next.unassign(key);
                }
            }
        }

        // A session attached to an external browser carries nothing that only
        // describes a local launch, so it cannot reach that browser: no state
        // file to replay into it, no proxy credentials to answer challenges
        // with. Inherited values are dropped, and reported as such.
        if next.connection() != Connection::Local {
            let engine = next.get(Key::Engine).and_then(Value::as_str);
            let drop_engine = matches!(
                next.connection(),
                Connection::Cdp(_) | Connection::AutoConnect
            ) && engine != Some(FIREFOX)
                || engine == Some(FIREFOX)
                    && next.connection() != self.connection()
                    && patch.value(Key::Engine).and_then(Value::as_str) != Some(FIREFOX);
            let engine = drop_engine.then_some(Key::Engine);
            for key in DROPPED_FOR_EXTERNAL.into_iter().chain(engine) {
                if let Some(current) = next.get(key).cloned() {
                    ignored.push(change(
                        key,
                        Requested::Default,
                        Some(current),
                        Reach::Launch,
                    ));
                    next.unassign(key);
                }
            }
        }

        let relaunch =
            patch.forces_relaunch() || changes.iter().any(|change| change.effect == Effect::Launch);
        Plan {
            changes,
            awaiting_engine,
            relaunch,
            next,
            ignored,
        }
    }

    /// Identifies the launch-shaping settings the browser was started with.
    /// Reported in `lifecycle` so a caller can see that two commands ran on the
    /// same launch; never compared to decide anything.
    pub fn launch_identity(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for s in SPECS.iter().filter(|s| s.effect == Effect::Launch) {
            if let Some(value) = self.get(s.key) {
                s.name.hash(&mut h);
                value.to_string().hash(&mut h);
            }
        }
        h.finish()
    }
}

/// The change one key makes, with the effect the browser gives it.
fn change(key: Key, requested: Requested, running: Option<Value>, reach: Reach) -> Change {
    Change {
        key,
        requested,
        running,
        effect: match reach {
            Reach::InPlace => Effect::Live,
            Reach::Ignored | Reach::Launch | Reach::Awaiting => spec(key).effect,
        },
    }
}

/// How the session `next` takes `key`: a local launch takes every setting as
/// its own, and an attached browser follows the table for its engine. Only a
/// `cdp` target the session has not attached to yet can still turn out to be
/// Firefox; a recorded engine belongs to the target it was detected on.
fn reach_for(next: &Running, running: &Running, key: Key, named: Option<&str>) -> Reach {
    let records_firefox = next.engine().as_deref() == Some(FIREFOX);
    let names_firefox = named == Some(FIREFOX);
    let new_target = next.connection() != running.connection();
    let engine = match next.connection() {
        Connection::Local => return Reach::Launch,
        _ if names_firefox || (records_firefox && !new_target) => Engine::Firefox,
        Connection::Cdp(_) if new_target => Engine::Undetected,
        _ => Engine::Other,
    };
    reach(key, engine, records_firefox || names_firefox)
}

/// Resets inherited settings that cannot coexist with the one just set. A
/// key the same patch sets is left alone: that conflict is the caller's.
fn displace(next: &mut Running, patch: &Patch, keys: &[Key], changes: &mut Vec<Change>) {
    for key in keys {
        if patch.get(*key).is_some() {
            continue;
        }
        if let Some(current) = next.get(*key).cloned() {
            changes.push(change(
                *key,
                Requested::Default,
                Some(current),
                Reach::Launch,
            ));
            next.unassign(*key);
        }
    }
}

fn displace_other_targets(next: &mut Running, patch: &Patch, set: Key, changes: &mut Vec<Change>) {
    let mut displaced: Vec<Key> = CONNECTION_KEYS
        .iter()
        .copied()
        .filter(|other| *other != set)
        .collect();
    // Headers the same command sends (env or config `cdpHeaders`) stay, or
    // every later command would see them as new and relaunch.
    if set != Key::Cdp && patch.get(Key::CdpHeaders).is_none() {
        displaced.push(Key::CdpHeaders);
    }
    for other in displaced {
        if let Some(current) = next.get(other).cloned() {
            changes.push(change(
                other,
                Requested::Default,
                Some(current),
                Reach::Launch,
            ));
            next.unassign(other);
        }
    }
}
