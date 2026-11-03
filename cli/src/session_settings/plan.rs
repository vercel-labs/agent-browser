//! Planning: what a patch changes in a running session.
//!
//! `Running::plan` is the only place settings are compared. It is pure: the
//! caller applies the plan and then stores `next`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

use super::patch::{Patch, Requested};
use super::running::Running;
use super::spec::{redacted, spec, Effect, Key, SPECS};

/// One setting whose requested value differs from the running one.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub key: Key,
    pub requested: Requested,
    /// The running value; `None` when the setting was at its default.
    pub running: Option<Value>,
}

impl Change {
    pub fn effect(&self) -> Effect {
        spec(self.key).effect
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
            "effect": match s.effect {
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
    /// The browser has to be relaunched: a launch setting changed or the
    /// invocation forced it.
    pub relaunch: bool,
    /// The settings after the plan is applied.
    pub next: Running,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && !self.relaunch
    }
}

/// The connection target keys; setting one displaces the others.
const CONNECTION_KEYS: [Key; 3] = [Key::Cdp, Key::AutoConnect, Key::Provider];

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
    /// - setting one connection target displaces the other targets.
    pub fn plan(&self, patch: &Patch) -> Plan {
        let mut next = self.clone();
        let mut changes: Vec<Change> = Vec::new();

        for (key, requested) in patch.iter() {
            match requested {
                Requested::Set(value) if key == Key::ClearCaCert => {
                    // A one-shot: drops the CA trust that is in effect.
                    if value == &Value::Bool(true) {
                        if let Some(current) = self.get(Key::CaCert) {
                            changes.push(Change {
                                key,
                                requested: requested.clone(),
                                running: Some(current.clone()),
                            });
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
                    changes.push(Change {
                        key,
                        requested: requested.clone(),
                        running: self.get(key).cloned(),
                    });
                    next.assign(key, value.clone());
                    if CONNECTION_KEYS.contains(&key) {
                        displace_other_targets(&mut next, key, &mut changes);
                    }
                    // Headers belong to the endpoint they were set for.
                    if key == Key::Cdp {
                        displace(&mut next, patch, &[Key::CdpHeaders], &mut changes);
                    }
                    // Storage state needs a clean browser, and a profile or
                    // extensions cannot be combined with it. The one set last
                    // wins, so an inherited value cannot wedge the session.
                    match key {
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
                    let Some(current) = self.get(key) else {
                        continue;
                    };
                    if default_value(key).as_ref() == Some(current) {
                        continue;
                    }
                    changes.push(Change {
                        key,
                        requested: Requested::Default,
                        running: Some(current.clone()),
                    });
                    next.unassign(key);
                }
            }
        }

        let relaunch = patch.forces_relaunch()
            || changes
                .iter()
                .any(|change| change.effect() == Effect::Launch);
        Plan {
            changes,
            relaunch,
            next,
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

/// Resets inherited settings that cannot coexist with the one just set. A
/// key the same patch sets is left alone: that conflict is the caller's.
fn displace(next: &mut Running, patch: &Patch, keys: &[Key], changes: &mut Vec<Change>) {
    for key in keys {
        if patch.get(*key).is_some() {
            continue;
        }
        if let Some(current) = next.get(*key).cloned() {
            changes.push(Change {
                key: *key,
                requested: Requested::Default,
                running: Some(current),
            });
            next.unassign(*key);
        }
    }
}

fn displace_other_targets(next: &mut Running, set: Key, changes: &mut Vec<Change>) {
    let mut displaced: Vec<Key> = CONNECTION_KEYS
        .iter()
        .copied()
        .filter(|other| *other != set)
        .collect();
    if set != Key::Cdp {
        displaced.push(Key::CdpHeaders);
    }
    for other in displaced {
        if let Some(current) = next.get(other).cloned() {
            changes.push(Change {
                key: other,
                requested: Requested::Default,
                running: Some(current),
            });
            next.unassign(other);
        }
    }
}
