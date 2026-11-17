//! Session settings: the option table, the patch an invocation sends, and the
//! typed views the daemon reads.
//!
//! The only IO is `store`, which keeps the security settings across daemon
//! restarts. `seed` is the one place that reads the environment, kept as a
//! temporary shim until the daemon stops being spawned with the settings in
//! its environment.

// Accessors the tests use have no non-test caller yet.
#[allow(dead_code)]
mod patch;
mod plan;
mod proxy;
mod running;
mod seed;
#[allow(dead_code)]
mod spec;
mod store;

pub use patch::{Patch, Requested};
pub use proxy::parse_proxy;
#[cfg(test)]
pub use running::resolve_idle_timeout;
pub use running::SecurityView;
pub use running::{named, Connection, IdleTimeout, Running, DEFAULT_IDLE_TIMEOUT_MS};
#[allow(unused_imports)]
pub use spec::{spec, spec_by_name, Effect, Key, Persist, Shape, Spec, SPECS};
pub use store::{clear_file, is_refusal, orphan_sessions, FsStore, Store};
#[cfg(test)]
pub use store::{MemStore, StoreError};

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
