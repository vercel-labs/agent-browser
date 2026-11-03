//! The option table: the only list of session settings.
//!
//! Adding an option is one `Key` variant plus one `SPECS` row. The tests fail
//! when a variant has no row or a row is out of order.

use serde_json::Value;

use crate::flags::Flags;

use super::patch::Requested;
use super::proxy::parse_proxy;

macro_rules! keys {
    ($($variant:ident),+ $(,)?) => {
        /// One session setting.
        #[allow(clippy::enum_variant_names)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Key {
            $($variant),+
        }

        impl Key {
            /// Every key, in declaration order.
            pub const ALL: &'static [Key] = &[$(Key::$variant),+];
        }
    };
}

keys! {
    // Connection target.
    Cdp, CdpHeaders, AutoConnect, Provider, Device,
    // Launch-shaping.
    Engine, Headed, Profile, Args, ExecutablePath, Extensions, Proxy, ProxyBypass,
    ProxyUsername, ProxyPassword, UserAgent, IgnoreHttpsErrors, AllowFileAccess,
    HideScrollbars, Webgpu, Webmcp, NoXvfb, CaCert, ClearCaCert, InitScripts, Enable,
    Plugins, StorageState, ColorScheme, DownloadPath,
    // Security.
    AllowedDomains, ActionPolicy, ConfirmActions,
    // Lifetimes and per-session behaviour.
    IdleTimeout, DefaultTimeout, NoAutoDialog, PinTab, Debug,
    RestoreKey, RestoreSave, RestoreCheckUrl, RestoreCheckText, RestoreCheckFn,
}

/// What changing a setting does to a running session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Needs a different browser: a launch or attach.
    Launch,
    /// Applies to the running session in place.
    Live,
}

/// How long a setting lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persist {
    /// Lost with the daemon.
    Memory,
    /// Survives daemon restarts.
    Secured,
}

/// The JSON shape a setting's value takes on the wire and in `Running`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Bool,
    Str,
    StrList,
    Millis,
    /// Structured value kept as-is (plugin configs).
    Json,
}

impl Shape {
    pub fn accepts(self, value: &Value) -> bool {
        match self {
            Shape::Bool => value.is_boolean(),
            Shape::Str => value.is_string(),
            Shape::StrList => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            Shape::Millis => value.is_u64(),
            Shape::Json => true,
        }
    }
}

pub struct Spec {
    pub key: Key,
    /// Wire name; also used in notices.
    pub name: &'static str,
    /// Command-line spelling; empty when the option has no flag.
    pub flag: &'static str,
    pub effect: Effect,
    pub persist: Persist,
    /// Stored and shown only as a digest once secured storage exists.
    pub secret: bool,
    pub shape: Shape,
    /// Reads the already-resolved flags; never the environment.
    pub read: fn(&Flags) -> Option<Requested>,
}

pub fn spec(key: Key) -> &'static Spec {
    &SPECS[key as usize]
}

pub fn spec_by_name(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.name == name)
}

/// A value safe to print: secret rows are hidden and the proxy loses any
/// credentials embedded in its URL.
pub(super) fn redacted(key: Key, value: &Value) -> Value {
    if spec(key).secret {
        return Value::from("<redacted>");
    }
    if key == Key::Proxy {
        if let Some(raw) = value.as_str() {
            return Value::from(parse_proxy(raw).server);
        }
    }
    value.clone()
}

fn set(value: Value) -> Option<Requested> {
    Some(Requested::Set(value))
}

fn string(value: &Option<String>) -> Option<Requested> {
    value
        .as_ref()
        .map(|v| Requested::Set(Value::from(v.as_str())))
}

fn list(values: &[String]) -> Option<Requested> {
    (!values.is_empty()).then(|| Requested::Set(Value::from(values.to_vec())))
}

/// An env-or-flag boolean that only counts when it is on.
fn on(value: bool) -> Option<Requested> {
    value.then_some(Requested::Set(Value::Bool(true)))
}

fn restore_key(f: &Flags) -> Option<&str> {
    f.restore.as_deref().or(f.session_name.as_deref())
}

fn read_cdp(f: &Flags) -> Option<Requested> {
    string(&f.cdp)
}
fn read_cdp_headers(f: &Flags) -> Option<Requested> {
    string(&f.cdp_headers)
}
fn read_auto_connect(f: &Flags) -> Option<Requested> {
    on(f.auto_connect)
}
fn read_provider(f: &Flags) -> Option<Requested> {
    string(&f.provider)
}
fn read_device(f: &Flags) -> Option<Requested> {
    string(&f.device)
}
fn read_engine(f: &Flags) -> Option<Requested> {
    string(&f.engine)
}
fn read_headed(f: &Flags) -> Option<Requested> {
    (f.headed || f.cli_headed).then_some(Requested::Set(Value::Bool(f.headed)))
}
fn read_profile(f: &Flags) -> Option<Requested> {
    string(&f.profile)
}
fn read_args(f: &Flags) -> Option<Requested> {
    f.args
        .as_deref()
        .map(|a| Requested::Set(Value::from(crate::flags::split_launch_args(a))))
}
fn read_executable_path(f: &Flags) -> Option<Requested> {
    string(&f.executable_path)
}
fn read_extensions(f: &Flags) -> Option<Requested> {
    list(&f.extensions)
}
fn read_proxy(f: &Flags) -> Option<Requested> {
    f.proxy
        .as_deref()
        .map(|p| Requested::Set(Value::from(parse_proxy(p).server)))
}
fn read_proxy_bypass(f: &Flags) -> Option<Requested> {
    match f.proxy_bypass.as_deref() {
        Some(bypass) => set(Value::from(bypass)),
        // Obscura takes the bypass from every launch, so absent is an
        // explicit clear.
        None if f.engine.as_deref() == Some("obscura") => Some(Requested::Default),
        None => None,
    }
}
fn read_proxy_username(f: &Flags) -> Option<Requested> {
    f.proxy
        .as_deref()
        .and_then(|p| parse_proxy(p).username)
        .map(|u| Requested::Set(Value::from(u)))
}
fn read_proxy_password(f: &Flags) -> Option<Requested> {
    f.proxy
        .as_deref()
        .and_then(|p| parse_proxy(p).password)
        .map(|p| Requested::Set(Value::from(p)))
}
fn read_user_agent(f: &Flags) -> Option<Requested> {
    string(&f.user_agent)
}
fn read_ignore_https_errors(f: &Flags) -> Option<Requested> {
    on(f.ignore_https_errors)
}
fn read_allow_file_access(f: &Flags) -> Option<Requested> {
    on(f.allow_file_access)
}
fn read_hide_scrollbars(f: &Flags) -> Option<Requested> {
    // Hiding is the default, so only an explicit choice or a turn-off is news.
    (f.cli_hide_scrollbars || !f.hide_scrollbars)
        .then_some(Requested::Set(Value::Bool(f.hide_scrollbars)))
}
fn read_webgpu(f: &Flags) -> Option<Requested> {
    (f.webgpu || f.cli_webgpu).then_some(Requested::Set(Value::Bool(f.webgpu)))
}
fn read_webmcp(f: &Flags) -> Option<Requested> {
    (f.no_webmcp || f.cli_no_webmcp).then_some(Requested::Set(Value::Bool(!f.no_webmcp)))
}
fn read_no_xvfb(f: &Flags) -> Option<Requested> {
    on(f.no_xvfb)
}
fn read_ca_cert(f: &Flags) -> Option<Requested> {
    string(&f.ca_cert)
}
fn read_clear_ca_cert(f: &Flags) -> Option<Requested> {
    on(f.clear_ca_cert)
}
fn read_init_scripts(f: &Flags) -> Option<Requested> {
    list(&f.init_scripts)
}
fn read_enable(f: &Flags) -> Option<Requested> {
    list(&f.enable)
}
fn read_plugins(f: &Flags) -> Option<Requested> {
    (!f.plugins.is_empty())
        .then(|| serde_json::to_value(&f.plugins).ok())
        .flatten()
        .map(Requested::Set)
}
fn read_storage_state(f: &Flags) -> Option<Requested> {
    string(&f.state)
}
fn read_color_scheme(f: &Flags) -> Option<Requested> {
    string(&f.color_scheme)
}
fn read_download_path(f: &Flags) -> Option<Requested> {
    string(&f.download_path)
}
fn read_allowed_domains(f: &Flags) -> Option<Requested> {
    f.allowed_domains
        .as_ref()
        .map(|d| Requested::Set(Value::from(d.clone())))
}
fn read_action_policy(f: &Flags) -> Option<Requested> {
    string(&f.action_policy)
}
fn read_confirm_actions(f: &Flags) -> Option<Requested> {
    string(&f.confirm_actions)
}
fn read_idle_timeout(f: &Flags) -> Option<Requested> {
    string(&f.idle_timeout)
}
fn read_default_timeout(f: &Flags) -> Option<Requested> {
    f.default_timeout.map(|ms| Requested::Set(Value::from(ms)))
}
fn read_no_auto_dialog(f: &Flags) -> Option<Requested> {
    on(f.no_auto_dialog)
}
fn read_pin_tab(f: &Flags) -> Option<Requested> {
    // An explicit disable has to reach a session that was pinned earlier.
    (f.pin_tab || f.cli_pin_tab).then_some(Requested::Set(Value::Bool(f.pin_tab)))
}
fn read_debug(f: &Flags) -> Option<Requested> {
    on(f.debug)
}
fn read_restore_key(f: &Flags) -> Option<Requested> {
    restore_key(f).map(|k| Requested::Set(Value::from(k)))
}
fn read_restore_save(f: &Flags) -> Option<Requested> {
    restore_key(f).map(|_| Requested::Set(Value::from(f.restore_save.as_deref().unwrap_or("auto"))))
}
/// Restore checks travel with a restore key; an unset check is an explicit
/// clear so a later check does not outlive its key.
fn restore_check(f: &Flags, value: &Option<String>) -> Option<Requested> {
    restore_key(f)?;
    Some(match value {
        Some(v) => Requested::Set(Value::from(v.as_str())),
        None => Requested::Default,
    })
}
fn read_restore_check_url(f: &Flags) -> Option<Requested> {
    restore_check(f, &f.restore_check_url)
}
fn read_restore_check_text(f: &Flags) -> Option<Requested> {
    restore_check(f, &f.restore_check_text)
}
fn read_restore_check_fn(f: &Flags) -> Option<Requested> {
    restore_check(f, &f.restore_check_fn)
}

macro_rules! row {
    ($key:ident, $name:literal, $flag:literal, $effect:ident, $persist:ident, $secret:literal, $shape:ident, $read:ident) => {
        Spec {
            key: Key::$key,
            name: $name,
            flag: $flag,
            effect: Effect::$effect,
            persist: Persist::$persist,
            secret: $secret,
            shape: Shape::$shape,
            read: $read,
        }
    };
}

/// One row per `Key`, in declaration order so `spec` can index by key.
pub static SPECS: &[Spec] = &[
    row!(Cdp, "cdp", "--cdp", Launch, Memory, false, Str, read_cdp),
    row!(
        CdpHeaders,
        "cdp-headers",
        "--cdp-headers",
        Launch,
        Memory,
        true,
        Str,
        read_cdp_headers
    ),
    row!(
        AutoConnect,
        "auto-connect",
        "--auto-connect",
        Launch,
        Memory,
        false,
        Bool,
        read_auto_connect
    ),
    row!(
        Provider,
        "provider",
        "--provider",
        Launch,
        Memory,
        false,
        Str,
        read_provider
    ),
    row!(
        Device,
        "device",
        "--device",
        Launch,
        Memory,
        false,
        Str,
        read_device
    ),
    row!(
        Engine,
        "engine",
        "--engine",
        Launch,
        Memory,
        false,
        Str,
        read_engine
    ),
    row!(
        Headed,
        "headed",
        "--headed",
        Launch,
        Memory,
        false,
        Bool,
        read_headed
    ),
    row!(
        Profile,
        "profile",
        "--profile",
        Launch,
        Memory,
        false,
        Str,
        read_profile
    ),
    row!(Args, "args", "--args", Launch, Memory, false, StrList, read_args),
    row!(
        ExecutablePath,
        "executable-path",
        "--executable-path",
        Launch,
        Memory,
        false,
        Str,
        read_executable_path
    ),
    row!(
        Extensions,
        "extensions",
        "--extension",
        Launch,
        Memory,
        false,
        StrList,
        read_extensions
    ),
    row!(Proxy, "proxy", "--proxy", Launch, Memory, false, Str, read_proxy),
    row!(
        ProxyBypass,
        "proxy-bypass",
        "--proxy-bypass",
        Launch,
        Memory,
        false,
        Str,
        read_proxy_bypass
    ),
    row!(
        ProxyUsername,
        "proxy-username",
        "--proxy",
        Launch,
        Memory,
        false,
        Str,
        read_proxy_username
    ),
    row!(
        ProxyPassword,
        "proxy-password",
        "--proxy",
        Launch,
        Memory,
        true,
        Str,
        read_proxy_password
    ),
    row!(
        UserAgent,
        "user-agent",
        "--user-agent",
        Launch,
        Memory,
        false,
        Str,
        read_user_agent
    ),
    row!(
        IgnoreHttpsErrors,
        "ignore-https-errors",
        "--ignore-https-errors",
        Launch,
        Memory,
        false,
        Bool,
        read_ignore_https_errors
    ),
    row!(
        AllowFileAccess,
        "allow-file-access",
        "--allow-file-access",
        Launch,
        Memory,
        false,
        Bool,
        read_allow_file_access
    ),
    row!(
        HideScrollbars,
        "hide-scrollbars",
        "--hide-scrollbars",
        Launch,
        Memory,
        false,
        Bool,
        read_hide_scrollbars
    ),
    row!(
        Webgpu,
        "webgpu",
        "--webgpu",
        Launch,
        Memory,
        false,
        Bool,
        read_webgpu
    ),
    row!(
        Webmcp,
        "webmcp",
        "--no-webmcp",
        Launch,
        Memory,
        false,
        Bool,
        read_webmcp
    ),
    row!(
        NoXvfb,
        "no-xvfb",
        "",
        Launch,
        Memory,
        false,
        Bool,
        read_no_xvfb
    ),
    row!(
        CaCert,
        "ca-cert",
        "--ca-cert",
        Launch,
        Memory,
        false,
        Str,
        read_ca_cert
    ),
    row!(
        ClearCaCert,
        "clear-ca-cert",
        "--no-ca-cert",
        Launch,
        Memory,
        false,
        Bool,
        read_clear_ca_cert
    ),
    row!(
        InitScripts,
        "init-scripts",
        "--init-script",
        Launch,
        Memory,
        false,
        StrList,
        read_init_scripts
    ),
    row!(
        Enable,
        "enable",
        "--enable",
        Launch,
        Memory,
        false,
        StrList,
        read_enable
    ),
    row!(
        Plugins,
        "plugins",
        "",
        Launch,
        Memory,
        false,
        Json,
        read_plugins
    ),
    row!(
        StorageState,
        "storage-state",
        "--state",
        Launch,
        Memory,
        false,
        Str,
        read_storage_state
    ),
    row!(
        ColorScheme,
        "color-scheme",
        "--color-scheme",
        Launch,
        Memory,
        false,
        Str,
        read_color_scheme
    ),
    row!(
        DownloadPath,
        "download-path",
        "--download-path",
        Launch,
        Memory,
        false,
        Str,
        read_download_path
    ),
    row!(
        AllowedDomains,
        "allowed-domains",
        "--allowed-domains",
        Launch,
        Secured,
        false,
        StrList,
        read_allowed_domains
    ),
    row!(
        ActionPolicy,
        "action-policy",
        "--action-policy",
        Live,
        Secured,
        false,
        Str,
        read_action_policy
    ),
    row!(
        ConfirmActions,
        "confirm-actions",
        "--confirm-actions",
        Live,
        Secured,
        false,
        Str,
        read_confirm_actions
    ),
    row!(
        IdleTimeout,
        "idle-timeout",
        "--idle-timeout",
        Live,
        Memory,
        false,
        Str,
        read_idle_timeout
    ),
    row!(
        DefaultTimeout,
        "default-timeout",
        "--default-timeout",
        Live,
        Memory,
        false,
        Millis,
        read_default_timeout
    ),
    row!(
        NoAutoDialog,
        "no-auto-dialog",
        "--no-auto-dialog",
        Live,
        Memory,
        false,
        Bool,
        read_no_auto_dialog
    ),
    row!(
        PinTab,
        "pin-tab",
        "--pin-tab",
        Live,
        Memory,
        false,
        Bool,
        read_pin_tab
    ),
    row!(Debug, "debug", "--debug", Live, Memory, false, Bool, read_debug),
    row!(
        RestoreKey,
        "restore-key",
        "--restore",
        Live,
        Memory,
        false,
        Str,
        read_restore_key
    ),
    row!(
        RestoreSave,
        "restore-save",
        "--restore-save",
        Live,
        Memory,
        false,
        Str,
        read_restore_save
    ),
    row!(
        RestoreCheckUrl,
        "restore-check-url",
        "--restore-check-url",
        Live,
        Memory,
        false,
        Str,
        read_restore_check_url
    ),
    row!(
        RestoreCheckText,
        "restore-check-text",
        "--restore-check-text",
        Live,
        Memory,
        false,
        Str,
        read_restore_check_text
    ),
    row!(
        RestoreCheckFn,
        "restore-check-fn",
        "--restore-check-fn",
        Live,
        Memory,
        false,
        Str,
        read_restore_check_fn
    ),
];
