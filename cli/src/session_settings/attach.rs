//! The one place that says where a setting can take effect: in the browser
//! this daemon launches, or in the browser it attached to.
//!
//! The planner reads `reach` to decide whether a patch changes the session,
//! waits for the attach, or is ignored, and the Firefox attach code applies
//! what `reach` calls in place.

use super::spec::Key;

/// The engine whose attached browser agent-browser drives over WebDriver BiDi.
pub(super) const FIREFOX: &str = "firefox";

/// Where a setting can take effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// Only in a browser this daemon launches.
    Launch,
    /// In any attached browser, once the session attaches again.
    Attach,
    /// In place in an attached Firefox; ignored in any other attached browser.
    FirefoxOnly,
    /// In place in an attached Firefox; otherwise once the session attaches again.
    FirefoxOrAttach,
    /// The engine selector, which an attach that drives Firefox has to record.
    Engine,
}

/// Where `key` can take effect.
pub fn placement(key: Key) -> Where {
    match key {
        Key::UserAgent => Where::FirefoxOnly,
        Key::DownloadPath => Where::FirefoxOrAttach,
        Key::Engine => Where::Engine,
        Key::AllowFileAccess
        | Key::Args
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
        | Key::Webgpu => Where::Launch,
        Key::Cdp
        | Key::CdpHeaders
        | Key::AutoConnect
        | Key::Provider
        | Key::Device
        | Key::IgnoreHttpsErrors
        | Key::Webmcp
        | Key::CaCert
        | Key::ClearCaCert
        | Key::InitScripts
        | Key::Enable
        | Key::Plugins
        | Key::ColorScheme
        | Key::Capture
        | Key::AllowedDomains
        | Key::ActionPolicy
        | Key::ConfirmActions
        | Key::IdleTimeout
        | Key::DefaultTimeout
        | Key::NoAutoDialog
        | Key::PinTab
        | Key::Debug
        | Key::RestoreKey
        | Key::RestoreSave
        | Key::RestoreCheckUrl
        | Key::RestoreCheckText
        | Key::RestoreCheckFn => Where::Attach,
    }
}

/// What a plan knows about the engine of the browser the session attaches to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Firefox, which agent-browser drives over WebDriver BiDi.
    Firefox,
    /// Any other browser, attached over CDP.
    Other,
    /// A `cdp` target the session has not attached to yet, which may turn out to be Firefox.
    Undetected,
}

/// What a session attached to a browser driven by `engine` does with a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Reported as ignored: nothing in the attached browser changes.
    Ignored,
    /// It takes effect through a launch, or through a fresh attach.
    Launch,
    /// The browser takes it in place, without reconnecting.
    InPlace,
    /// Carried into the attach, which keeps it for an engine that takes it and
    /// reports it as ignored otherwise.
    Awaiting,
}

/// How the browser this session attaches to takes `key`, where `selects_firefox` says
/// whether the session records Firefox as its engine or the request names it.
pub fn reach(key: Key, engine: Engine, selects_firefox: bool) -> Reach {
    match placement(key) {
        Where::Launch => Reach::Ignored,
        Where::Attach => Reach::Launch,
        Where::FirefoxOnly => match engine {
            Engine::Firefox => Reach::InPlace,
            Engine::Other => Reach::Ignored,
            Engine::Undetected => Reach::Awaiting,
        },
        Where::FirefoxOrAttach => match engine {
            Engine::Firefox => Reach::InPlace,
            Engine::Other | Engine::Undetected => Reach::Launch,
        },
        Where::Engine if selects_firefox => Reach::Launch,
        Where::Engine => Reach::Ignored,
    }
}
