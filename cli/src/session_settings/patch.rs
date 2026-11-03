//! A patch: the settings one invocation set explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde_json::{Map, Value};

use crate::flags::Flags;

use super::spec::{redacted, spec, spec_by_name, Key, SPECS};
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
}

/// Wire name of the forced-relaunch marker. It is not a setting, so it has no
/// row in the table.
const RELAUNCH_FIELD: &str = "relaunch";

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
        map.finish()
    }
}

impl Patch {
    /// Reads the resolved flags once per invocation. Pure.
    pub fn from_flags(flags: &Flags) -> Patch {
        let entries = SPECS
            .iter()
            .filter_map(|s| (s.read)(flags).map(|requested| (s.key, requested)))
            .collect();
        Patch {
            entries,
            relaunch: flags.relaunch,
            changed_in_place: BTreeSet::new(),
        }
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

    /// Drops a key the patch would otherwise carry.
    pub fn remove(&mut self, key: Key) {
        self.entries.remove(&key);
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
        Some(Value::Object(object))
    }

    pub fn from_wire(wire: Option<&Value>) -> Result<Patch, SettingsError> {
        let Some(wire) = wire else {
            return Ok(Patch::default());
        };
        let object = wire.as_object().ok_or(SettingsError::NotAnObject)?;
        let mut entries = BTreeMap::new();
        let mut relaunch = false;
        for (name, value) in object {
            if name == RELAUNCH_FIELD {
                relaunch = value.as_bool().ok_or_else(|| {
                    SettingsError::UnknownKey(format!("{} (expected a boolean)", name))
                })?;
                continue;
            }
            let s = spec_by_name(name).ok_or_else(|| SettingsError::UnknownKey(name.clone()))?;
            let requested = if value.is_null() {
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
        Ok(Patch {
            entries,
            relaunch,
            changed_in_place: BTreeSet::new(),
        })
    }

    /// A patch that sets the given keys. Used for tests and for translating
    /// a daemon-side launch command.
    pub fn from_entries(entries: impl IntoIterator<Item = (Key, Requested)>) -> Patch {
        Patch {
            entries: entries.into_iter().collect(),
            relaunch: false,
            changed_in_place: BTreeSet::new(),
        }
    }
}
