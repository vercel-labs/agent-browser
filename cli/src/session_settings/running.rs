//! The daemon's view of its session's settings.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

use crate::native::cdp::chrome::LaunchOptions;

use super::spec::{redacted, spec, Key};

/// Applied when no default timeout is set. Intentionally below the CLI's 30s
/// IPC read timeout so the daemon reports a proper timeout error instead of
/// the client dying with EAGAIN and retrying.
pub const DEFAULT_TIMEOUT_MS: u64 = 25_000;

/// Idle timeout applied when none is set, so an integration that dies without
/// calling `close` cannot leak the daemon and its Chrome tree indefinitely.
/// Socket commands and dashboard input reset the timer. Unlike an explicit
/// timeout, the default never closes a headed browser (including Safari and
/// iOS WebDriver sessions) or a user-attached browser because those may be in
/// direct human use that the daemon cannot observe. Provider-owned CDP
/// browsers remain eligible for cleanup.
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleTimeout {
    pub ms: u64,
    /// True when the value is `DEFAULT_IDLE_TIMEOUT_MS` rather than an
    /// explicit setting. Only the default exempts headed and user-attached
    /// browsers from shutdown.
    pub is_default: bool,
}

/// Resolve the idle timeout setting: unset or unparseable gives the default,
/// an explicit 0 disables it (`None`), any other value is that many
/// milliseconds.
pub fn resolve_idle_timeout(raw: Option<String>) -> Option<IdleTimeout> {
    match raw.as_deref().map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(ms)) => Some(IdleTimeout {
            ms,
            is_default: false,
        }),
        // Unparseable values are validated (with a warning) at the flags
        // layer; falling back to the default keeps the leak backstop in place
        // rather than silently disabling it.
        Some(Err(_)) | None => Some(IdleTimeout {
            ms: DEFAULT_IDLE_TIMEOUT_MS,
            is_default: true,
        }),
    }
}

/// Where the session's browser comes from. Precedence: cdp, auto-connect,
/// provider, local.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Connection {
    Local,
    Cdp(String),
    AutoConnect,
    Provider(String),
}

/// Settings that contain what the session may do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityView {
    /// Present whenever a list was set, even an empty one.
    pub allowed_domains: Option<Vec<String>>,
    pub action_policy_path: Option<String>,
    /// Raw comma-separated categories.
    pub confirm_actions: Option<String>,
}

/// Restore (auth state save and load) settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreView {
    pub key: Option<String>,
    pub save: String,
    pub check_url: Option<String>,
    pub check_text: Option<String>,
    pub check_fn: Option<String>,
}

/// Values the running session holds. An absent key means the default.
#[derive(Clone, Default, PartialEq)]
pub struct Running {
    values: BTreeMap<Key, Value>,
    /// Verbatim extensions value for the session's extensions file.
    /// Temporary with the env seed.
    pub(super) extensions_file: Option<String>,
    /// Headed as the plugin provider path reads it: any value except `0`,
    /// `false`, `no` or empty counts. Temporary with the env seed.
    pub(super) provider_headed: bool,
}

impl fmt::Debug for Running {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut map = f.debug_map();
        for (key, value) in &self.values {
            map.entry(&spec(*key).name, &redacted(*key, value));
        }
        map.finish()
    }
}

impl Running {
    /// A session with every setting at its default.
    pub fn defaults() -> Running {
        Running::default()
    }

    pub fn set(&mut self, key: Key, value: Value) {
        self.values.insert(key, value);
    }

    pub fn get(&self, key: Key) -> Option<&Value> {
        self.values.get(&key)
    }

    pub(super) fn remove(&mut self, key: Key) {
        self.values.remove(&key);
    }

    /// Stores a value from a plan and keeps the temporary env-seed fields in
    /// step with it.
    pub(super) fn assign(&mut self, key: Key, value: Value) {
        match key {
            Key::Extensions => {
                let items: Vec<&str> = value
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                self.extensions_file = (!items.is_empty()).then(|| items.join(","));
            }
            Key::Headed => self.provider_headed = value.as_bool().unwrap_or(false),
            _ => {}
        }
        self.values.insert(key, value);
    }

    /// Puts a setting back to a value it had, or to its default.
    pub fn put_back(&mut self, key: Key, value: Option<Value>) {
        match value {
            Some(value) => self.assign(key, value),
            None => self.unassign(key),
        }
    }

    /// Resets a setting to its default.
    pub(super) fn unassign(&mut self, key: Key) {
        match key {
            Key::Extensions => self.extensions_file = None,
            Key::Headed => self.provider_headed = false,
            _ => {}
        }
        self.values.remove(&key);
    }

    fn str(&self, key: Key) -> Option<String> {
        self.get(key).and_then(Value::as_str).map(String::from)
    }

    fn bool(&self, key: Key) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    fn list(&self, key: Key) -> Option<Vec<String>> {
        self.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
    }

    pub fn launch_options(&self) -> LaunchOptions {
        LaunchOptions {
            headless: !self.headed(),
            executable_path: self.str(Key::ExecutablePath),
            proxy: self.str(Key::Proxy),
            proxy_bypass: self.str(Key::ProxyBypass),
            proxy_username: self.str(Key::ProxyUsername),
            proxy_password: self.str(Key::ProxyPassword),
            profile: self.str(Key::Profile),
            allow_file_access: self.bool(Key::AllowFileAccess).unwrap_or(false),
            args: self.list(Key::Args).unwrap_or_default(),
            extensions: self.list(Key::Extensions),
            storage_state: self.str(Key::StorageState),
            user_agent: self.str(Key::UserAgent),
            ignore_https_errors: self.bool(Key::IgnoreHttpsErrors).unwrap_or(false),
            ca_cert: None,
            ca_bundle: None,
            ca_cert_digest: None,
            prepared_nss_home: None,
            color_scheme: self.str(Key::ColorScheme),
            download_path: self.str(Key::DownloadPath),
            hide_scrollbars: self.hide_scrollbars(),
            viewport_size: None,
            use_real_keychain: false,
            webgpu: self.bool(Key::Webgpu).unwrap_or(false),
            webmcp: self.webmcp(),
            no_xvfb: self.no_xvfb(),
            restrict_webrtc: self.get(Key::AllowedDomains).is_some(),
        }
    }

    pub fn connection(&self) -> Connection {
        if let Some(cdp) = self.str(Key::Cdp) {
            Connection::Cdp(cdp)
        } else if self.get(Key::AutoConnect).is_some() {
            Connection::AutoConnect
        } else if let Some(provider) = self.str(Key::Provider) {
            Connection::Provider(provider)
        } else {
            Connection::Local
        }
    }

    pub fn default_timeout_ms(&self) -> u64 {
        self.get(Key::DefaultTimeout)
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
    }

    pub fn idle_timeout(&self) -> Option<IdleTimeout> {
        resolve_idle_timeout(self.str(Key::IdleTimeout))
    }

    pub fn security(&self) -> SecurityView {
        SecurityView {
            allowed_domains: self.list(Key::AllowedDomains),
            action_policy_path: self.str(Key::ActionPolicy),
            confirm_actions: self.str(Key::ConfirmActions),
        }
    }

    pub fn restore(&self) -> RestoreView {
        RestoreView {
            key: self.str(Key::RestoreKey),
            save: self
                .str(Key::RestoreSave)
                .unwrap_or_else(|| "auto".to_string()),
            check_url: self.str(Key::RestoreCheckUrl),
            check_text: self.str(Key::RestoreCheckText),
            check_fn: self.str(Key::RestoreCheckFn),
        }
    }

    pub fn engine(&self) -> Option<String> {
        self.str(Key::Engine)
    }

    pub fn headed(&self) -> bool {
        self.bool(Key::Headed).unwrap_or(false)
    }

    pub fn hide_scrollbars(&self) -> bool {
        self.bool(Key::HideScrollbars).unwrap_or(true)
    }

    pub fn webmcp(&self) -> bool {
        self.bool(Key::Webmcp).unwrap_or(true)
    }

    pub fn no_xvfb(&self) -> bool {
        self.bool(Key::NoXvfb).unwrap_or(false)
    }

    /// The raw provider name, whatever else is set.
    pub fn provider(&self) -> Option<String> {
        self.str(Key::Provider)
    }

    pub fn user_agent(&self) -> Option<String> {
        self.str(Key::UserAgent)
    }

    pub fn color_scheme(&self) -> Option<String> {
        self.str(Key::ColorScheme)
    }

    /// Headed as handed to plugin providers. Looser than `headed()`, which
    /// accepts only `1` and `true`: `yes` or `on` count here.
    pub fn plugin_provider_headed(&self) -> bool {
        self.provider_headed
    }

    pub fn pin_tab(&self) -> bool {
        self.bool(Key::PinTab).unwrap_or(false)
    }

    pub fn no_auto_dialog(&self) -> bool {
        self.bool(Key::NoAutoDialog).unwrap_or(false)
    }

    /// Built-in init scripts such as `react-devtools`.
    pub fn enable_features(&self) -> Vec<String> {
        self.list(Key::Enable).unwrap_or_default()
    }

    /// User-provided init script paths.
    pub fn init_scripts(&self) -> Vec<String> {
        self.list(Key::InitScripts).unwrap_or_default()
    }

    /// Raw JSON object of WebSocket headers for a `cdp` connection.
    pub fn cdp_headers(&self) -> Option<String> {
        self.str(Key::CdpHeaders)
    }

    /// Verbatim extensions value for the session's extensions file.
    pub fn extensions_file_contents(&self) -> Option<&str> {
        self.extensions_file.as_deref()
    }
}
