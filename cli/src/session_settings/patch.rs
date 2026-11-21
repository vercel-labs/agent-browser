//! A patch: the settings one invocation set explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};

use crate::flags::Flags;

use super::spec::{redacted, spec, spec_by_name, Effect, Key, Persist, SPECS};
use super::SettingsError;

/// What an invocation asks for one setting.
#[derive(Clone, Debug, PartialEq)]
pub enum Requested {
    Set(Value),
    /// An explicit reset to the default (for example a cleared proxy bypass).
    Default,
}

/// Only what this invocation set. An absent key defers to the running value.
#[derive(Clone, Default, PartialEq)]
pub struct Patch {
    entries: BTreeMap<Key, Requested>,
    /// Relaunch the browser even when no setting changed (`--relaunch`).
    relaunch: bool,
    /// Keys whose requested value equals the running one but whose content
    /// differs (a CA file edited in place). Daemon-side only: never sent.
    changed_in_place: BTreeSet<Key>,
    /// Connection entries that came from the environment or config rather
    /// than the command line. They are session defaults: the plan applies them
    /// only to a session that has no connection target yet.
    ambient: BTreeSet<Key>,
}

/// Wire name of the forced-relaunch marker. It is not a setting, so it has no
/// row in the table.
const RELAUNCH_FIELD: &str = "relaunch";

/// Wire name of the list of ambient connection entries. Absent means every
/// entry is explicit, which is what a CLI without the field sends.
const AMBIENT_FIELD: &str = "ambient";

/// The settings an environment or config value can make ambient.
pub(super) const AMBIENT_KEYS: [Key; 4] =
    [Key::Cdp, Key::AutoConnect, Key::Provider, Key::CdpHeaders];

/// The connection targets of one invocation after the environment and config
/// defaults are resolved against the command line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Targets {
    pub cdp: Option<String>,
    pub provider: Option<String>,
    pub auto_connect: bool,
}

/// Which targets an invocation names. A target on the command line wins and
/// drops the environment and config ones. Without one, a blank default names
/// nothing and a single default is kept, picked by precedence: cdp, then
/// auto-connect, then provider. A blank `-p ""` is kept: it resets the session.
pub fn effective_targets(flags: &Flags) -> Targets {
    let blank = |v: &String| v.trim().is_empty();
    let mut cdp = flags.cdp.clone();
    let mut provider = flags.provider.clone();
    let mut auto_connect = flags.auto_connect;
    if !flags.cli_cdp && cdp.as_ref().is_some_and(blank) {
        cdp = None;
    }
    if !flags.cli_provider && provider.as_ref().is_some_and(blank) {
        provider = None;
    }
    let explicit = flags.cli_cdp && cdp.is_some()
        || flags.cli_provider && provider.as_ref().is_some_and(|v| !blank(v))
        || flags.cli_auto_connect && auto_connect;
    if explicit {
        if !flags.cli_cdp {
            cdp = None;
        }
        if !flags.cli_provider {
            provider = None;
        }
        auto_connect &= flags.cli_auto_connect;
    } else if cdp.is_some() {
        auto_connect = false;
        if !flags.cli_provider {
            provider = None;
        }
    } else if auto_connect && !flags.cli_provider {
        provider = None;
    }
    Targets {
        cdp,
        provider,
        auto_connect,
    }
}

impl fmt::Debug for Patch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (key, requested) in &self.entries {
            match requested {
                Requested::Set(v) => map.entry(&spec(*key).name, &redacted(*key, v)),
                Requested::Default => map.entry(&spec(*key).name, &"<default>"),
            };
        }
        if self.relaunch {
            map.entry(&RELAUNCH_FIELD, &true);
        }
        if !self.ambient.is_empty() {
            let names: Vec<&str> = self.ambient.iter().map(|k| spec(*k).name).collect();
            map.entry(&AMBIENT_FIELD, &names);
        }
        map.finish()
    }
}

impl Patch {
    /// Reads the resolved flags once per invocation. Pure.
    pub fn from_flags(flags: &Flags) -> Patch {
        let mut entries: BTreeMap<Key, Requested> = SPECS
            .iter()
            .filter_map(|s| (s.read)(flags).map(|requested| (s.key, requested)))
            .collect();
        let targets = effective_targets(flags);
        let mut ambient = BTreeSet::new();
        for key in AMBIENT_KEYS {
            let (kept, explicit) = match key {
                Key::Cdp => (targets.cdp.is_some(), flags.cli_cdp),
                Key::Provider => (targets.provider.is_some(), flags.cli_provider),
                // Headers belong to an endpoint: the plan decides whether
                // this invocation sends one.
                Key::CdpHeaders => (
                    flags.cli_cdp_headers
                        || flags.cdp_headers.as_deref().is_some_and(|v| {
                            // Defaults belong to the endpoint they name: not
                            // to a different one given with `--cdp`.
                            !v.trim().is_empty()
                                && (!flags.cli_cdp
                                    || targets
                                        .cdp
                                        .as_deref()
                                        .is_none_or(|cdp| flags.default_headers_fit(cdp)))
                        }),
                    flags.cli_cdp_headers,
                ),
                _ => (
                    targets.auto_connect || flags.no_auto_connect || flags.cli_auto_connect,
                    flags.cli_auto_connect || flags.no_auto_connect,
                ),
            };
            if !kept {
                entries.remove(&key);
            } else if !explicit && entries.contains_key(&key) {
                ambient.insert(key);
            }
        }
        Patch {
            entries,
            relaunch: flags.relaunch,
            changed_in_place: BTreeSet::new(),
            ambient,
        }
    }

    /// Whether the entry for `key` came from the environment or config.
    pub fn is_ambient(&self, key: Key) -> bool {
        self.ambient.contains(&key)
    }

    /// What a command that names its own endpoint sends: the targets from the
    /// environment or config are dropped, and the headers from there go with
    /// the endpoint it names.
    pub fn without_ambient_connection(&self) -> Patch {
        let mut patch = self.clone();
        for key in std::mem::take(&mut patch.ambient) {
            if key != Key::CdpHeaders {
                patch.entries.remove(&key);
            }
        }
        patch
    }

    fn retain_ambient(&mut self) {
        let entries = &self.entries;
        self.ambient.retain(|key| entries.contains_key(key));
    }

    pub fn is_noop(&self) -> bool {
        self.entries.is_empty() && !self.relaunch
    }

    /// Whether the invocation asked for a relaunch regardless of changes.
    pub fn forces_relaunch(&self) -> bool {
        self.relaunch
    }

    /// Marks a key as changed even if its value equals the running one.
    pub fn mark_changed_in_place(&mut self, key: Key) {
        self.changed_in_place.insert(key);
    }

    pub fn is_changed_in_place(&self, key: Key) -> bool {
        self.changed_in_place.contains(&key)
    }

    /// The patch without its launch settings and without a forced relaunch:
    /// what a command that never launches a browser can apply.
    pub fn without_launch_settings(&self) -> Patch {
        let mut live = self.clone();
        live.relaunch = false;
        live.entries
            .retain(|key, _| spec(*key).effect == Effect::Live);
        live.retain_ambient();
        live
    }

    /// The patch without the security settings that apply at once (action
    /// policy, confirm actions): what a confirmation replay may apply, so it
    /// cannot put back a policy that changed after the command was parked.
    /// Allowed domains stay: they apply only at the launch the replay runs.
    pub fn without_live_security_settings(&self) -> Patch {
        let mut patch = self.clone();
        patch.entries.retain(|key, _| {
            let spec = spec(*key);
            !(spec.persist == Persist::Secured && spec.effect == Effect::Live)
        });
        patch
    }

    /// Drops the forced relaunch, for a command that follows one that carried it.
    pub fn clear_relaunch(&mut self) {
        self.relaunch = false;
    }

    /// Drops a key the patch would otherwise carry.
    pub fn remove(&mut self, key: Key) {
        self.entries.remove(&key);
        self.ambient.remove(&key);
    }

    pub fn get(&self, key: Key) -> Option<&Requested> {
        self.entries.get(&key)
    }

    /// The value set for `key`, if the patch sets one.
    pub fn value(&self, key: Key) -> Option<&Value> {
        match self.entries.get(&key) {
            Some(Requested::Set(v)) => Some(v),
            _ => None,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Key, &Requested)> {
        self.entries.iter().map(|(k, r)| (*k, r))
    }

    /// The `settings` field of a command; `None` when there is nothing to say.
    pub fn to_wire(&self) -> Option<Value> {
        if self.is_noop() {
            return None;
        }
        let mut object: Map<String, Value> = self
            .entries
            .iter()
            .map(|(key, requested)| {
                let value = match requested {
                    Requested::Set(v) => v.clone(),
                    Requested::Default => Value::Null,
                };
                (spec(*key).name.to_string(), value)
            })
            .collect();
        if self.relaunch {
            object.insert(RELAUNCH_FIELD.to_string(), Value::Bool(true));
        }
        if !self.ambient.is_empty() {
            let names: Vec<Value> = self
                .ambient
                .iter()
                .map(|k| Value::from(spec(*k).name))
                .collect();
            object.insert(AMBIENT_FIELD.to_string(), Value::Array(names));
        }
        Some(Value::Object(object))
    }

    pub fn from_wire(wire: Option<&Value>) -> Result<Patch, SettingsError> {
        let Some(wire) = wire else {
            return Ok(Patch::default());
        };
        let object = wire.as_object().ok_or(SettingsError::NotAnObject)?;
        let mut entries = BTreeMap::new();
        let mut relaunch = false;
        let mut ambient = BTreeSet::new();
        for (name, value) in object {
            if name == AMBIENT_FIELD {
                let names = value.as_array().ok_or_else(|| {
                    SettingsError::UnknownKey(format!("{} (expected a list of names)", name))
                })?;
                for item in names {
                    let key = item
                        .as_str()
                        .and_then(spec_by_name)
                        .map(|s| s.key)
                        .filter(|key| AMBIENT_KEYS.contains(key))
                        .ok_or_else(|| SettingsError::UnknownKey(format!("{} {}", name, item)))?;
                    ambient.insert(key);
                }
                continue;
            }
            if name == RELAUNCH_FIELD {
                relaunch = value.as_bool().ok_or_else(|| {
                    SettingsError::UnknownKey(format!("{} (expected a boolean)", name))
                })?;
                continue;
            }
            let s = spec_by_name(name).ok_or_else(|| SettingsError::UnknownKey(name.clone()))?;
            let requested = if value.is_null() {
                if s.persist == Persist::Secured {
                    // Containment is never cleared by a bare null; a reset
                    // needs an explicit flag.
                    return Err(SettingsError::Invalid(
                        s.key,
                        "a security setting cannot be cleared this way".to_string(),
                    ));
                }
                Requested::Default
            } else if s.shape.accepts(value) {
                Requested::Set(value.clone())
            } else {
                return Err(SettingsError::Invalid(
                    s.key,
                    format!("expected {:?}", s.shape),
                ));
            };
            entries.insert(s.key, requested);
        }
        let mut patch = Patch {
            entries,
            relaunch,
            changed_in_place: BTreeSet::new(),
            ambient,
        };
        patch.retain_ambient();
        Ok(patch)
    }

    /// A patch that sets the given keys. Used for tests and for translating
    /// a daemon-side launch command.
    pub fn from_entries(entries: impl IntoIterator<Item = (Key, Requested)>) -> Patch {
        Patch {
            entries: entries.into_iter().collect(),
            relaunch: false,
            changed_in_place: BTreeSet::new(),
            ambient: BTreeSet::new(),
        }
    }
}
