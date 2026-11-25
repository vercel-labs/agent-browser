//! Planning: what a patch changes in a running session.
//!
//! `Running::plan` is the only place settings are compared. It is pure: the
//! caller applies the plan and then stores `next`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

use super::attach::{Destination, Engine, Reach, Transition, FIREFOX};
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
    /// The attached browser takes the change in place, so it changes a browser
    /// the session does not own.
    pub in_place: bool,
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

    /// The session-level live settings, taken from `before` so nothing a launch
    /// would carry moves into the running settings. Changes an attached browser
    /// takes in place are left out: they change a browser the session does not
    /// own, so they wait for `in_place_only`.
    pub fn live_only(&self, before: &Running) -> Plan {
        self.filtered(before, self.ignored.clone(), |change| {
            change.effect == Effect::Live && !change.in_place
        })
    }

    /// The changes an attached browser takes in place, taken from `before`.
    pub fn in_place_only(&self, before: &Running) -> Plan {
        self.filtered(before, Vec::new(), |change| change.in_place)
    }

    /// The part of the plan whose changes `keep` selects, applied to `before`.
    fn filtered(
        &self,
        before: &Running,
        ignored: Vec<Change>,
        keep: impl Fn(&Change) -> bool,
    ) -> Plan {
        let mut next = before.clone();
        let mut changes = Vec::new();
        for change in self.changes.iter().filter(|change| keep(change)) {
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
            ignored,
        }
    }
}

/// The connection target keys; setting one displaces the others.
const CONNECTION_KEYS: [Key; 3] = [Key::Cdp, Key::AutoConnect, Key::Provider];

const DROPPED_FOR_EXTERNAL: [Key; 11] = [
    Key::StorageState,
    Key::Profile,
    Key::Extensions,
    Key::Proxy,
    Key::ProxyBypass,
    Key::ProxyUsername,
    Key::ProxyPassword,
    Key::ExecutablePath,
    Key::Args,
    Key::AllowFileAccess,
    Key::Webgpu,
];

const DROPPED_FOR_ATTACH: [Key; 3] = [Key::Headed, Key::NoXvfb, Key::HideScrollbars];

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

    /// The browser this patch lands the session on, decided before any setting
    /// is, so no reach decision depends on the order the patch names keys.
    pub fn destination(&self, patch: &Patch) -> Destination {
        let named_firefox = patch.value(Key::Engine).and_then(Value::as_str) == Some(FIREFOX);
        let mut targets = self.clone();
        for key in CONNECTION_KEYS {
            if !patch.is_ambient(key) && !target_is_free(self, patch, key) {
                apply_target(&mut targets, patch, key);
            }
        }
        let ambient_targets = targets.connection() == Connection::Local;
        if ambient_targets {
            for key in CONNECTION_KEYS {
                if patch.is_ambient(key) && !target_is_free(&targets, patch, key) {
                    apply_target(&mut targets, patch, key);
                }
            }
        }
        let to = targets.connection();
        let moved = to != self.connection();
        let transition = if moved || patch.forces_relaunch() {
            Transition::Replace
        } else {
            Transition::Reuse
        };
        let records_firefox = match patch.get(Key::Engine) {
            Some(Requested::Set(value)) => value.as_str() == Some(FIREFOX),
            Some(Requested::Default) => false,
            None => self.engine().as_deref() == Some(FIREFOX),
        };
        let engine = if named_firefox || (records_firefox && !moved) {
            Engine::Firefox
        } else if moved && matches!(to, Connection::Cdp(_)) {
            Engine::Undetected
        } else {
            Engine::Other
        };
        Destination {
            transition,
            to,
            engine,
            selects_firefox: named_firefox || self.engine().as_deref() == Some(FIREFOX),
            ambient_targets,
        }
    }

    /// Plans `patch` for the destination the session and the patch name.
    pub fn plan(&self, patch: &Patch) -> Plan {
        let destination = self.destination(patch);
        self.plan_for(&destination, patch)
    }

    /// Plans `patch` for a destination whose engine the caller already knows.
    #[cfg(test)]
    pub fn plan_with_engine(&self, patch: &Patch, engine: Engine) -> Plan {
        let destination = self.destination(patch).with_engine(engine);
        self.plan_for(&destination, patch)
    }

    /// Plans a patch for a detected engine and transition.
    pub fn plan_with_transition(
        &self,
        patch: &Patch,
        engine: Engine,
        transition: Transition,
    ) -> Plan {
        let mut destination = self.destination(patch).with_engine(engine);
        destination.transition = transition;
        self.plan_for(&destination, patch)
    }

    /// Plans `patch`. Rules:
    /// - a key the patch does not name never produces a change;
    /// - a value equal to the running one (or to the default when nothing is
    ///   running) is free: no change, no notice, no relaunch;
    /// - a setting an attached browser cannot take is ignored, as `attach` says;
    /// - a setting only an attached Firefox takes is carried into an attach whose
    ///   engine is not known yet, as `Plan::awaiting_engine` records;
    /// - setting one connection target displaces the other targets;
    /// - an ambient connection entry applies only to a session with no target.
    fn plan_for(&self, destination: &Destination, patch: &Patch) -> Plan {
        let mut next = self.clone();
        let mut changes: Vec<Change> = Vec::new();
        let mut awaiting_engine: Vec<Change> = Vec::new();
        let mut ignored: Vec<Change> = Vec::new();

        // Connection entries from the environment or config are session
        // defaults: they apply after the explicit ones, and only to a session
        // that has no target by then. Headers are credentials of an endpoint,
        // so they resolve after every target, and only for an endpoint this
        // command sends or one the session already runs.
        let ambient_headers = |key: &Key| *key == Key::CdpHeaders;
        let ordered = patch
            .iter()
            .filter(|(key, _)| !patch.is_ambient(*key) && !ambient_headers(key))
            .chain(
                patch
                    .iter()
                    .filter(|(key, _)| patch.is_ambient(*key) && !ambient_headers(key)),
            )
            .chain(patch.iter().filter(|(key, _)| ambient_headers(key)));
        for (key, requested) in ordered {
            if ambient_headers(&key) {
                // Headers go to the endpoint the invocation names, and to no
                // other. When that endpoint was skipped because the session
                // stays on another one, headers for it are dropped rather than
                // sent to the endpoint that remains. Headers named by no
                // endpoint update the attached one when they were given on the
                // command line; headers from the environment or config have
                // none to belong to.
                let send = match patch.get(Key::Cdp) {
                    Some(Requested::Set(endpoint)) => next.get(Key::Cdp) == Some(endpoint),
                    Some(Requested::Default) => false,
                    None => !patch.is_ambient(Key::CdpHeaders),
                };
                if !send {
                    continue;
                }
            } else if patch.is_ambient(key) && !destination.ambient_targets {
                continue;
            }
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
                    // A default is checked against the session as this patch
                    // left it: a reset earlier in the patch may have cleared
                    // the running value, and the default then applies again.
                    let current = if patch.is_ambient(key) {
                        next.effective(key)
                    } else {
                        self.effective(key)
                    };
                    let reach = destination.reach(key);
                    let carried =
                        destination.replaces() && matches!(reach, Reach::InPlace | Reach::Ignored);
                    if current.as_ref() == Some(value)
                        && !patch.is_changed_in_place(key)
                        && !carried
                    {
                        continue;
                    }
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
                        for (other, current) in clear_other_targets(&mut next, patch, key) {
                            changes.push(change(
                                other,
                                Requested::Default,
                                Some(current),
                                Reach::Launch,
                            ));
                        }
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
                    let reach = destination.reach(key);
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
        if destination.is_external() {
            let previous_engine = self.engine();
            let records = match patch.get(Key::Engine) {
                Some(Requested::Set(value)) => value.as_str(),
                Some(Requested::Default) => None,
                None => previous_engine.as_deref(),
            };
            let names_firefox = patch.value(Key::Engine).and_then(Value::as_str) == Some(FIREFOX);
            let switched = destination.to != self.connection();
            let drop_engine = destination.engine != Engine::Firefox
                && (matches!(destination.to, Connection::Cdp(_) | Connection::AutoConnect)
                    && records != Some(FIREFOX)
                    || records == Some(FIREFOX) && switched && !names_firefox);
            let attach_only: &[Key] = if matches!(destination.to, Connection::Provider(_)) {
                &[]
            } else {
                &DROPPED_FOR_ATTACH
            };
            let dropped = DROPPED_FOR_EXTERNAL
                .iter()
                .copied()
                .chain(attach_only.iter().copied())
                .chain(drop_engine.then_some(Key::Engine));
            for key in dropped {
                if let Some(current) = next.get(key).cloned() {
                    if !ignored.iter().any(|change| change.key == key) {
                        ignored.push(change(
                            key,
                            Requested::Default,
                            Some(current),
                            Reach::Launch,
                        ));
                    }
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
        in_place: reach == Reach::InPlace,
    }
}

/// Whether the patch sets a connection target to the value the session already
/// has, which the planner leaves alone.
fn target_is_free(next: &Running, patch: &Patch, key: Key) -> bool {
    match patch.get(key) {
        Some(Requested::Set(value)) => {
            next.effective(key).as_ref() == Some(value) && !patch.is_changed_in_place(key)
        }
        _ => false,
    }
}

/// Sets or clears one connection target, dropping the entries it displaces.
fn apply_target(next: &mut Running, patch: &Patch, key: Key) {
    match patch.get(key) {
        Some(Requested::Set(value)) => {
            next.assign(key, value.clone());
            clear_other_targets(next, patch, key);
        }
        Some(Requested::Default) => next.unassign(key),
        None => {}
    }
}

/// Drops the connection entries a target displaces: the other targets, and
/// headers that belonged to the endpoint that went away.
fn clear_other_targets(next: &mut Running, patch: &Patch, set: Key) -> Vec<(Key, Value)> {
    let mut displaced: Vec<Key> = CONNECTION_KEYS
        .iter()
        .copied()
        .filter(|other| *other != set)
        .collect();
    if set != Key::Cdp && patch.get(Key::CdpHeaders).is_none() {
        displaced.push(Key::CdpHeaders);
    }
    let mut cleared = Vec::new();
    for key in displaced {
        if let Some(current) = next.get(key).cloned() {
            next.unassign(key);
            cleared.push((key, current));
        }
    }
    cleared
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
