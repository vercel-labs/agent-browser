//! Session settings: the option table, the patch an invocation sends, and the
//! typed views the daemon reads.
//!
//! The module does no IO. `seed` is the one place that reads the environment,
//! kept as a temporary shim until the patch rides on every command.

// The patch and wire halves have no caller until the CLI sends them.
#![allow(dead_code)]

mod patch;
mod proxy;
mod running;
mod seed;
mod spec;

#[allow(unused_imports)]
pub use patch::{Patch, Requested};
#[allow(unused_imports)]
pub use proxy::{parse_proxy, ParsedProxy};
#[allow(unused_imports)]
pub use running::{
    resolve_idle_timeout, Connection, IdleTimeout, Running, SecurityView, DEFAULT_IDLE_TIMEOUT_MS,
    DEFAULT_TIMEOUT_MS,
};
#[allow(unused_imports)]
pub use spec::{spec, spec_by_name, Effect, Key, Persist, Shape, Spec, SPECS};

use std::fmt;

#[derive(Debug, PartialEq)]
pub enum SettingsError {
    NotAnObject,
    UnknownKey(String),
    Invalid(Key, String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SettingsError::NotAnObject => write!(f, "settings must be a JSON object"),
            SettingsError::UnknownKey(name) => write!(f, "unknown setting '{}'", name),
            SettingsError::Invalid(key, why) => {
                write!(f, "invalid value for {}: {}", spec(*key).name, why)
            }
        }
    }
}

impl std::error::Error for SettingsError {}

#[cfg(test)]
pub(crate) mod tests;
