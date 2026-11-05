//! Planning: what a patch changes in a running session.
//!
//! `Running::plan` is the only place settings are compared. It is pure: the
//! caller applies the plan and then stores `next`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

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
}

impl Change {
    pub fn effect(&self) -> Effect {
        spec(self.key).effect
    }

    /// An ignored request as reported to the caller.
    pub fn to_ignored_json(&self) -> Value {
        let mut report = self.to_json(false);
        report["effect"] = json!("ignored");
        report["reason"] = json!(
            "applies only to a browser agent-browser launches itself, not to a CDP, auto-connect or provider session"
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
    /// Requested values that only describe a browser agent-browser launches
    /// itself, for a session attached to an external browser. They change
    /// nothing and never relaunch.
    pub ignored: Vec<Change>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && !self.relaunch
    }
}

/// The connection target keys; setting one displaces the others.
const CONNECTION_KEYS: [Key; 3] = [Key::Cdp, Key::AutoConnect, Key::Provider];

/// Settings that describe a browser agent-browser starts itself. A session
/// attached to an external browser ignores them, as the launch commands the CLI
/// used to send never carried them for those sessions.
fn is_local_only(key: Key) -> bool {
    matches!(
        key,
        Key::AllowFileAccess
            | Key::Args
            | Key::Engine
            | Key::ExecutablePath
            | Key::Extensions
            | Key::Headed
            | Key::HideScrollbars
            | Key::NoXvfb
            | Key::Profile
            | Key::Proxy
            | Key::ProxyBypass
            | Key::ProxyUsername
            | Key::ProxyPassword
            | Key::StorageState
            | Key::UserAgent
            | Key::Webgpu
    )
}

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
    /// - setting one connection target displaces the other targets.
    pub fn plan(&self, patch: &Patch) -> Plan {
        let mut next = self.clone();
        let mut changes: Vec<Change> = Vec::new();
        let mut ignored: Vec<Change> = Vec::new();

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
                    if is_local_only(key) && next.connection() != Connection::Local {
                        ignored.push(Change {
                            key,
                            requested: requested.clone(),
                            running: self.get(key).cloned(),
                        });
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
                    // Read `next`: a target set earlier in this patch may have
                    // displaced the key already, and that change is recorded.
                    let Some(current) = next.get(key).cloned() else {
                        continue;
                    };
                    let current = &current;
                    if default_value(key).as_ref() == Some(current) {
                        continue;
                    }
                    if is_local_only(key) && next.connection() != Connection::Local {
                        ignored.push(Change {
                            key,
                            requested: Requested::Default,
                            running: Some(current.clone()),
                        });
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

        // A session attached to an external browser carries nothing that only
        // describes a local launch, so it cannot reach that browser: no state
        // file to replay into it, no proxy credentials to answer challenges
        // with. Inherited values are dropped, and reported as such.
        if next.connection() != Connection::Local {
            // A CDP or auto-connect browser is not one agent-browser picks an
            // engine for; plugin providers do read it.
            let engine = matches!(
                next.connection(),
                Connection::Cdp(_) | Connection::AutoConnect
            )
            .then_some(Key::Engine);
            for key in DROPPED_FOR_EXTERNAL.into_iter().chain(engine) {
                if let Some(current) = next.get(key).cloned() {
                    ignored.push(Change {
                        key,
                        requested: Requested::Default,
                        running: Some(current),
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
