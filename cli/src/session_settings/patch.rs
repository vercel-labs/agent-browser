//! A patch: the settings one invocation set explicitly.

use std::collections::BTreeMap;
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
        Patch { entries }
    }

    pub fn is_noop(&self) -> bool {
        self.entries.is_empty()
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
        let object: Map<String, Value> = self
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
        Some(Value::Object(object))
    }

    pub fn from_wire(wire: Option<&Value>) -> Result<Patch, SettingsError> {
        let Some(wire) = wire else {
            return Ok(Patch::default());
        };
        let object = wire.as_object().ok_or(SettingsError::NotAnObject)?;
        let mut entries = BTreeMap::new();
        for (name, value) in object {
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
        Ok(Patch { entries })
    }
}
