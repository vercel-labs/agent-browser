use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::Duration;

use futures_util::future::join_all;
use serde_json::{json, Map, Value};

use super::client::BidiClient;
use super::daemon::session;
use super::firefox_devtools::RdpClient;
use super::session::FirefoxSession;
use crate::native::actions::{DaemonState, EmulatedMedia, SessionSetup, ViewportSetup};

type Viewport = (i64, i64, f64);

pub(crate) type Command = (&'static str, Value);

/// Emulation applied to the cookie jar of the active tab, so a tab a page opens has it too, or
/// to the session's tabs alone in a browser it attached to.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub viewport: Option<Viewport>,
    pub(super) window_viewports: HashMap<String, Viewport>,
    pub user_agent: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub geolocation: Option<(f64, f64, Option<f64>)>,
    pub offline: Option<bool>,
    pub media: Option<EmulatedMedia>,
    /// True for a browser the session attached to, whose other tabs are not its to change.
    pub tab_scoped: bool,
}

/// Where an override applies.
#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> {
    /// Every tab of a cookie jar, including one a page opens there later.
    Jar(&'a str),
    /// One tab only.
    Tab,
}

impl Settings {
    /// A browser the session owns takes overrides per cookie jar; one it attached to, per tab.
    fn scope<'a>(&self, jar: &'a str) -> Scope<'a> {
        if self.tab_scoped {
            Scope::Tab
        } else {
            Scope::Jar(jar)
        }
    }
}

impl From<&SessionSetup> for Settings {
    fn from(setup: &SessionSetup) -> Self {
        Self {
            viewport: setup.viewport.map(|viewport| {
                (
                    i64::from(viewport.width),
                    i64::from(viewport.height),
                    viewport.device_scale_factor,
                )
            }),
            user_agent: setup.user_agent.clone(),
            locale: setup.locale.clone(),
            timezone: setup.timezone.clone(),
            geolocation: setup.geolocation,
            offline: setup.offline,
            media: setup.emulated_media.clone(),
            ..Self::default()
        }
    }
}

/// Holds live emulation and serializes viewport writes from commands and context events.
#[derive(Default)]
pub struct Emulation {
    pub(super) settings: RwLock<Settings>,
    viewport_gate: tokio::sync::Mutex<()>,
}

impl Emulation {
    pub(super) fn new(settings: Settings) -> Self {
        Self {
            settings: RwLock::new(settings),
            ..Self::default()
        }
    }

    pub(super) fn forget_viewports(&self, contexts: &HashSet<String>) {
        if !contexts.is_empty() {
            self.settings
                .write()
                .unwrap()
                .window_viewports
                .retain(|context, _| !contexts.contains(context));
        }
    }
}

/// Maps a viewport size and device pixel ratio to `browsingContext.setViewport`.
pub(crate) fn viewport(width: i64, height: i64, device_pixel_ratio: f64) -> Command {
    (
        "browsingContext.setViewport",
        json!({
            "viewport": { "width": width, "height": height },
            "devicePixelRatio": device_pixel_ratio,
        }),
    )
}

/// Maps a user agent string to `emulation.setUserAgentOverride`.
pub(crate) fn user_agent(user_agent: &str) -> Command {
    (
        "emulation.setUserAgentOverride",
        json!({ "userAgent": user_agent }),
    )
}

/// Maps a locale to `emulation.setLocaleOverride`.
pub(crate) fn locale(locale: &str) -> Command {
    ("emulation.setLocaleOverride", json!({ "locale": locale }))
}

/// Maps a timezone id to `emulation.setTimezoneOverride`.
pub(crate) fn timezone(timezone: &str) -> Command {
    (
        "emulation.setTimezoneOverride",
        json!({ "timezone": timezone }),
    )
}

/// Maps geolocation coordinates to `emulation.setGeolocationOverride`.
pub(crate) fn geolocation(latitude: f64, longitude: f64, accuracy: Option<f64>) -> Command {
    (
        "emulation.setGeolocationOverride",
        json!({
            "coordinates": {
                "latitude": latitude,
                "longitude": longitude,
                "accuracy": accuracy.unwrap_or(1.0),
            },
        }),
    )
}

/// Maps offline mode to `emulation.setNetworkConditions`.
pub(crate) fn offline(enabled: bool) -> Command {
    (
        "emulation.setNetworkConditions",
        json!({
            "networkConditions": if enabled {
                json!({ "type": "offline" })
            } else {
                Value::Null
            },
        }),
    )
}

/// Validates media emulation and maps it to DevTools `targetConfiguration` values.
pub(crate) fn validate_media(media: &EmulatedMedia) -> Result<Value, String> {
    let mut config = Map::new();
    if let Some(media_type) = &media.media {
        match media_type.to_lowercase().as_str() {
            "print" => {
                config.insert("printSimulationEnabled".to_string(), Value::Bool(true));
            }
            "screen" => {
                config.insert("printSimulationEnabled".to_string(), Value::Bool(false));
            }
            other => {
                return Err(format!(
                    "Unsupported media type '{other}'. Supported media types: print, screen"
                ));
            }
        }
    }
    for (name, value) in &media.features {
        if value.trim().is_empty() || value.eq_ignore_ascii_case("no-preference") {
            continue;
        }
        match name.as_str() {
            "prefers-color-scheme" => {
                let scheme = value.to_ascii_lowercase();
                if color_scheme_pref(&scheme).is_none() {
                    return Err(format!(
                        "Unsupported color scheme '{value}'. Supported color schemes: dark, light"
                    ));
                }
                config.insert("colorSchemeSimulation".to_string(), Value::String(scheme));
            }
            other => {
                return Err(format!(
                    "Unsupported media feature '{other}'. Supported media features: prefers-color-scheme"
                ));
            }
        }
    }
    Ok(Value::Object(config))
}

/// Adds the reset values so one call replaces the previous media emulation completely.
fn media_config(media: &EmulatedMedia) -> Result<Value, String> {
    let mut config = validate_media(media)?;
    let object = config
        .as_object_mut()
        .ok_or("Media configuration must be an object")?;
    object.entry("colorSchemeSimulation").or_insert(Value::Null);
    object
        .entry("printSimulationEnabled")
        .or_insert(Value::Bool(false));
    Ok(config)
}

/// Sends an emulation command for the tab `context` in the scope the session uses.
async fn send_scoped(ff: &FirefoxSession, context: &str, command: Command) -> Result<(), String> {
    let scope = ff.emulation.settings.read().unwrap().scope(ff.contexts.jar(context));
    send_scoped_bidi(&ff.bidi, context, scope, command).await
}

/// Scopes `command`: a viewport always to the tab `context` it sizes, the rest to `scope`. A
/// cookie jar covers a tab a page opens there before its first request.
fn scoped(context: &str, scope: Scope<'_>, (method, mut params): Command) -> Command {
    match scope {
        _ if method == "browsingContext.setViewport" => params["context"] = json!(context),
        Scope::Jar(jar) => params["userContexts"] = json!([jar]),
        Scope::Tab => params["contexts"] = json!([context]),
    }
    (method, params)
}

async fn send_scoped_bidi(
    bidi: &BidiClient,
    context: &str,
    scope: Scope<'_>,
    command: Command,
) -> Result<(), String> {
    let (method, params) = scoped(context, scope, command);
    bidi.send(method, params).await.map(|_| ())
}

/// Sends `command` to each tab on its own and returns every tab's result in order, so a tab
/// Firefox will not let the session change, such as a privileged page, does not stop the others.
pub(crate) async fn send_to_tabs(
    bidi: &BidiClient,
    tabs: &[String],
    command: &Command,
    timeout: Duration,
) -> Vec<Result<(), String>> {
    join_all(tabs.iter().map(|tab| async move {
        let (method, params) = scoped(tab, Scope::Tab, command.clone());
        bidi.send_timeout(method, params, timeout).await.map(|_| ())
    }))
    .await
}

/// The commands that clear every override in `settings`, each addressed to one tab by
/// `send_to_tabs`, so a browser the session does not own gets its own values back. Media
/// emulation is not listed because Firefox ends it as soon as the DevTools connection closes
/// with the session.
pub(crate) fn resets(settings: &Settings) -> Vec<Command> {
    let shared = [
        (
            settings.user_agent.is_some(),
            "emulation.setUserAgentOverride",
            "userAgent",
        ),
        (
            settings.locale.is_some(),
            "emulation.setLocaleOverride",
            "locale",
        ),
        (
            settings.timezone.is_some(),
            "emulation.setTimezoneOverride",
            "timezone",
        ),
        (
            settings.geolocation.is_some(),
            "emulation.setGeolocationOverride",
            "coordinates",
        ),
        (
            settings.offline == Some(true),
            "emulation.setNetworkConditions",
            "networkConditions",
        ),
    ];
    let viewports = settings.viewport.is_some().then(viewport_reset);
    let shared = shared
        .into_iter()
        .filter(|(set, ..)| *set)
        .map(|(_, method, field)| (method, json!({ field: null })));
    viewports.into_iter().chain(shared).collect()
}

fn viewport_reset() -> Command {
    (
        "browsingContext.setViewport",
        json!({ "viewport": null, "devicePixelRatio": null }),
    )
}

pub(crate) async fn reset_window_viewports(
    bidi: &BidiClient,
    settings: &Settings,
    timeout: Duration,
) -> Vec<Result<(), String>> {
    join_all(settings.window_viewports.keys().map(|context| async move {
        let (method, params) = scoped(context, Scope::Tab, viewport_reset());
        bidi.send_timeout(method, params, timeout).await.map(|_| ())
    }))
    .await
}

const COLOR_SCHEME_PREF: &str = "layout.css.prefers-color-scheme.content-override";

/// The value Firefox's content color scheme preference takes for `scheme`.
fn color_scheme_pref(scheme: &str) -> Option<i64> {
    match scheme {
        "dark" => Some(0),
        "light" => Some(1),
        _ => None,
    }
}

/// Sets the color scheme every page of the browser reports, or follows the system for `None`.
/// It is a browser preference, so a tab a page opens has it before it loads.
async fn set_color_scheme(rdp: &RdpClient, scheme: Option<&str>) -> Result<(), String> {
    let root = rdp.request("root", "getRoot", json!({})).await?;
    let preferences = root["preferenceActor"]
        .as_str()
        .ok_or("Firefox DevTools did not expose the preference actor")?;
    match scheme.and_then(color_scheme_pref) {
        Some(value) => {
            let pref = json!({"name": COLOR_SCHEME_PREF, "value": value});
            rdp.request(preferences, "setIntPref", pref).await?;
        }
        None => {
            let pref = json!({"name": COLOR_SCHEME_PREF});
            rdp.request(preferences, "clearUserPref", pref).await?;
        }
    }
    Ok(())
}

/// Applies a media configuration. A browser the session owns takes the color scheme as a
/// preference for every tab and print simulation for the selected tab. One it attached to takes
/// both for the selected tab only, which leaves the user's preferences alone.
async fn apply_media(ff: &FirefoxSession, context: &str, config: &Value) -> Result<(), String> {
    if ff.emulation.settings.read().unwrap().tab_scoped {
        return update_tab_configuration(&ff.bidi, &ff.rdp, context, config).await;
    }
    set_color_scheme(&ff.rdp, config["colorSchemeSimulation"].as_str()).await?;
    let print = json!({"printSimulationEnabled": config["printSimulationEnabled"]});
    update_tab_configuration(&ff.bidi, &ff.rdp, context, &print).await
}

/// Applies a media configuration to the selected tab through its DevTools configuration actor.
pub(crate) async fn update_tab_configuration(
    bidi: &BidiClient,
    rdp: &RdpClient,
    context: &str,
    config: &Value,
) -> Result<(), String> {
    bidi.send("browsingContext.activate", json!({"context": context}))
        .await?;
    let tabs = rdp.tabs().await?;
    let descriptor = tabs
        .iter()
        .find(|tab| tab.selected)
        .or_else(|| tabs.iter().find(|tab| tab.loaded))
        .map(|tab| tab.actor.as_str())
        .ok_or("Firefox reported no tabs for target configuration")?;
    let watcher = rdp.request(descriptor, "getWatcher", json!({})).await?;
    let watcher_actor = watcher["actor"]
        .as_str()
        .ok_or("Firefox did not return a watcher actor")?;
    let configuration = rdp
        .request(watcher_actor, "getTargetConfigurationActor", json!({}))
        .await?;
    let actor = configuration["configuration"]["actor"]
        .as_str()
        .ok_or("Firefox did not return the target configuration actor")?;
    rdp.request(
        actor,
        "updateConfiguration",
        json!({"configuration": config}),
    )
    .await?;
    Ok(())
}

/// Replays every stored setting onto `context` as it becomes the active tab, including the
/// print simulation Firefox only accepts for the selected tab.
pub async fn apply_to_context(ff: &FirefoxSession, context: &str) -> Result<(), String> {
    apply_overrides(&ff.bidi, &ff.emulation, context, ff.contexts.jar(context)).await?;
    let media = ff.emulation.settings.read().unwrap().media.clone();
    if let Some(ref media) = media {
        apply_media(ff, context, &media_config(media)?).await?;
    }
    Ok(())
}

/// Replays the overrides onto the tab `context` or its cookie jar `jar`, whichever the session
/// uses, and the viewport onto `context`. The media configuration is left to `apply_to_context`.
pub(crate) async fn apply_overrides(
    bidi: &BidiClient,
    emulation: &Emulation,
    context: &str,
    jar: &str,
) -> Result<(), String> {
    let settings = {
        let _gate = emulation.viewport_gate.lock().await;
        let settings = emulation.settings.read().unwrap().clone();
        if let Some(size) = settings
            .window_viewports
            .get(context)
            .copied()
            .or(settings.viewport)
        {
            send_viewport(bidi, context, size).await?;
        }
        settings
    };
    let mut commands = Vec::new();
    if let Some(ref user_agent) = settings.user_agent {
        commands.push(self::user_agent(user_agent));
    }
    if let Some(ref locale) = settings.locale {
        commands.push(self::locale(locale));
    }
    if let Some(ref timezone) = settings.timezone {
        commands.push(self::timezone(timezone));
    }
    if let Some((latitude, longitude, accuracy)) = settings.geolocation {
        commands.push(geolocation(latitude, longitude, accuracy));
    }
    if let Some(offline) = settings.offline {
        commands.push(self::offline(offline));
    }
    for command in commands {
        send_scoped_bidi(bidi, context, settings.scope(jar), command).await?;
    }
    Ok(())
}

fn parse_viewport(cmd: &Value) -> Result<Viewport, String> {
    if cmd.get("mobile").and_then(Value::as_bool).unwrap_or(false) {
        return Err("Firefox does not support mobile viewport emulation".to_string());
    }
    let width = cmd.get("width").and_then(Value::as_i64).unwrap_or(1280);
    let height = cmd.get("height").and_then(Value::as_i64).unwrap_or(720);
    let scale = cmd
        .get("deviceScaleFactor")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    Ok((width, height, scale))
}

async fn send_viewport(
    bidi: &BidiClient,
    context: &str,
    (width, height, scale): Viewport,
) -> Result<(), String> {
    let (method, mut params) = viewport(width, height, scale);
    params["context"] = json!(context);
    bidi.send(method, params).await.map(|_| ())
}

async fn update_stream_viewport(state: &DaemonState, (width, height, _): Viewport) {
    if let Some(ref server) = state.stream_server {
        server.set_viewport(width as u32, height as u32).await;
    }
}

/// Sets a desktop viewport and remembers it for new tabs and owned Firefox relaunches.
pub async fn set_viewport(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let size @ (width, height, scale) = parse_viewport(cmd)?;
    {
        let ff = session(state)?;
        let context = ff.contexts.active_tab();
        let _gate = ff.emulation.viewport_gate.lock().await;
        send_viewport(&ff.bidi, context, size).await?;
        let mut settings = ff.emulation.settings.write().unwrap();
        settings.viewport = Some(size);
        settings.window_viewports.remove(context);
    }
    state.session_setup.viewport = Some(ViewportSetup {
        width: width as i32,
        height: height as i32,
        device_scale_factor: scale,
        mobile: false,
    });
    update_stream_viewport(state, size).await;
    if let Some(ref server) = state.stream_server {
        server
            .broadcast_status(true, false, width as u32, height as u32, "firefox")
            .await;
    }
    Ok(json!({
        "width": width,
        "height": height,
        "deviceScaleFactor": scale,
        "mobile": false,
    }))
}

/// Sizes one window context without changing the viewport inherited by other contexts.
pub async fn set_context_viewport(
    cmd: &Value,
    state: &mut DaemonState,
    context: &str,
) -> Result<(), String> {
    let size = parse_viewport(cmd)?;
    {
        let ff = session(state)?;
        let _gate = ff.emulation.viewport_gate.lock().await;
        send_viewport(&ff.bidi, context, size).await?;
        ff.emulation
            .settings
            .write()
            .unwrap()
            .window_viewports
            .insert(context.to_string(), size);
    }
    update_stream_viewport(state, size).await;
    Ok(())
}

/// Overrides the user agent for the active tab and any tab opened later.
pub async fn set_user_agent(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let user_agent = cmd
        .get("userAgent")
        .and_then(Value::as_str)
        .ok_or("Missing 'userAgent' parameter")?;
    let context = session(state)?.contexts.active_tab().to_string();
    send_scoped(session(state)?, &context, self::user_agent(user_agent)).await?;
    session(state)?
        .emulation
        .settings
        .write()
        .unwrap()
        .user_agent = Some(user_agent.to_string());
    state.session_setup.user_agent = Some(user_agent.to_string());
    Ok(json!({ "userAgent": user_agent }))
}

/// Overrides the locale for the active tab and any tab opened later.
pub async fn set_locale(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let locale = cmd
        .get("locale")
        .and_then(Value::as_str)
        .ok_or("Missing 'locale' parameter")?;
    let context = session(state)?.contexts.active_tab().to_string();
    send_scoped(session(state)?, &context, self::locale(locale)).await?;
    session(state)?.emulation.settings.write().unwrap().locale = Some(locale.to_string());
    state.session_setup.locale = Some(locale.to_string());
    Ok(json!({ "locale": locale }))
}

/// Overrides the timezone for the active tab and any tab opened later.
pub async fn set_timezone(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timezone = cmd
        .get("timezoneId")
        .or_else(|| cmd.get("timezone"))
        .and_then(Value::as_str)
        .ok_or("Missing 'timezoneId' parameter")?;
    let context = session(state)?.contexts.active_tab().to_string();
    send_scoped(session(state)?, &context, self::timezone(timezone)).await?;
    session(state)?.emulation.settings.write().unwrap().timezone = Some(timezone.to_string());
    state.session_setup.timezone = Some(timezone.to_string());
    Ok(json!({ "timezoneId": timezone }))
}

/// Overrides geolocation for the active tab and any tab opened later.
pub async fn set_geolocation(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let latitude = cmd
        .get("latitude")
        .and_then(Value::as_f64)
        .ok_or("Missing 'latitude' parameter")?;
    let longitude = cmd
        .get("longitude")
        .and_then(Value::as_f64)
        .ok_or("Missing 'longitude' parameter")?;
    let accuracy = cmd.get("accuracy").and_then(Value::as_f64);
    let context = session(state)?.contexts.active_tab().to_string();
    send_scoped(
        session(state)?,
        &context,
        geolocation(latitude, longitude, accuracy),
    )
    .await?;
    session(state)?
        .emulation
        .settings
        .write()
        .unwrap()
        .geolocation = Some((latitude, longitude, accuracy));
    state.session_setup.geolocation = Some((latitude, longitude, accuracy));
    Ok(json!({ "latitude": latitude, "longitude": longitude }))
}

/// Toggles offline mode for the active tab and any tab opened later.
pub async fn set_offline(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let offline = cmd.get("offline").and_then(Value::as_bool).unwrap_or(true);
    let context = session(state)?.contexts.active_tab().to_string();
    send_scoped(session(state)?, &context, self::offline(offline)).await?;
    session(state)?.emulation.settings.write().unwrap().offline = Some(offline);
    state.session_setup.offline = offline.then_some(true);
    Ok(json!({ "offline": offline }))
}

/// Applies the color scheme and print media, to every tab where the session owns the browser.
pub async fn set_media(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let media = parse_media(cmd);
    let config = media_config(&media)?;
    {
        let ff = session(state)?;
        let context = ff.contexts.active_tab().to_string();
        apply_media(ff, &context, &config).await?;
    }
    session(state)?.emulation.settings.write().unwrap().media = Some(media.clone());
    state.session_setup.emulated_media = Some(media);
    Ok(json!({ "set": true }))
}

fn parse_media(cmd: &Value) -> EmulatedMedia {
    let media = cmd.get("media").and_then(Value::as_str);
    let mut features = Vec::new();
    if let Some(scheme) = cmd.get("colorScheme").and_then(Value::as_str) {
        features.push(("prefers-color-scheme".to_string(), scheme.to_string()));
    }
    if let Some(motion) = cmd.get("reducedMotion").and_then(Value::as_str) {
        features.push(("prefers-reduced-motion".to_string(), motion.to_string()));
    }
    if let Some(object) = cmd.get("features").and_then(Value::as_object) {
        for (name, value) in object {
            features.push((name.clone(), value.as_str().unwrap_or("").to_string()));
        }
    }
    EmulatedMedia {
        media: media.map(String::from),
        features,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::native::bidi::session::tests::{fake_session, fake_session_with_rdp};
    use crate::native::bidi::tabs;
    use crate::native::stream::{IdleActivity, StreamServer};
    use futures_util::StreamExt;

    fn media_with_feature(name: &str, value: &str) -> EmulatedMedia {
        EmulatedMedia {
            media: None,
            features: vec![(name.to_string(), value.to_string())],
        }
    }

    #[test]
    fn color_scheme_dark_maps_to_color_scheme_simulation() {
        let config = validate_media(&media_with_feature("prefers-color-scheme", "dark")).unwrap();
        assert_eq!(config["colorSchemeSimulation"], "dark");
    }

    #[test]
    fn reduced_motion_lists_supported_features() {
        let error = validate_media(&media_with_feature("prefers-reduced-motion", "reduce"))
            .expect_err("reduced motion is not supported");
        assert!(error.contains("prefers-reduced-motion"));
        assert!(error.contains("prefers-color-scheme"));
    }

    #[test]
    fn offline_toggles_network_conditions() {
        let (method, params) = offline(true);
        assert_eq!(method, "emulation.setNetworkConditions");
        assert_eq!(params["networkConditions"]["type"], "offline");

        let (method, params) = offline(false);
        assert_eq!(method, "emulation.setNetworkConditions");
        assert!(params["networkConditions"].is_null());
    }

    #[test]
    fn resets_clear_each_override_without_naming_a_tab() {
        let settings = Settings {
            user_agent: Some("Agent/1.0".to_string()),
            offline: Some(true),
            ..Settings::default()
        };
        assert_eq!(
            resets(&settings),
            vec![
                (
                    "emulation.setUserAgentOverride",
                    json!({ "userAgent": null })
                ),
                (
                    "emulation.setNetworkConditions",
                    json!({ "networkConditions": null })
                ),
            ]
        );

        let settings = Settings {
            viewport: Some((800, 600, 2.0)),
            ..Settings::default()
        };
        assert_eq!(
            resets(&settings),
            vec![(
                "browsingContext.setViewport",
                json!({ "viewport": null, "devicePixelRatio": null })
            )]
        );
    }

    #[tokio::test]
    async fn window_viewport_resets_name_only_their_recorded_contexts() {
        let (ff, _, _, bidi, _) = fake_session_with_rdp().await;
        let mut settings = Settings::default();
        settings
            .window_viewports
            .insert("ctx-window".to_string(), (400, 500, 1.0));
        settings
            .window_viewports
            .insert("ctx-other".to_string(), (300, 200, 2.0));

        reset_window_viewports(&ff.bidi, &settings, Duration::from_secs(1)).await;

        let messages = bidi.lock().unwrap().clone();
        let viewports: Vec<&Value> = messages
            .iter()
            .filter(|m| m["method"] == "browsingContext.setViewport")
            .collect();
        let mut contexts: Vec<&str> = viewports
            .iter()
            .map(|m| m["params"]["context"].as_str().unwrap())
            .collect();
        contexts.sort();
        assert_eq!(contexts, vec!["ctx-other", "ctx-window"]);
        for message in viewports {
            assert!(message["params"]["viewport"].is_null());
            assert!(message["params"]["devicePixelRatio"].is_null());
            assert!(message["params"].get("contexts").is_none());
            assert!(message["params"].get("userContexts").is_none());
        }
        assert!(resets(&settings).is_empty());
    }

    async fn session_state() -> (DaemonState, crate::native::bidi::session::tests::Calls) {
        let (ff, _, calls) = fake_session().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        (state, calls)
    }

    #[tokio::test]
    async fn viewport_routes_to_bidi_and_returns_the_chrome_shape() {
        let (mut state, calls) = session_state().await;
        let (server, _) = StreamServer::start_without_client(
            0,
            "viewport-test".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .unwrap();
        let server = Arc::new(server);
        let (mut client, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/", server.port()))
                .await
                .unwrap();
        let initial = client.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&initial).unwrap()["type"],
            "status"
        );
        state.stream_server = Some(Arc::clone(&server));

        let result = set_viewport(
            &json!({"width": 800, "height": 600, "deviceScaleFactor": 2.0}),
            &mut state,
        )
        .await
        .unwrap();

        assert_eq!(
            result,
            json!({"width": 800, "height": 600, "deviceScaleFactor": 2.0, "mobile": false})
        );
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec!["browsingContext.setViewport"]
        );
        assert_eq!(
            state
                .firefox
                .as_ref()
                .unwrap()
                .emulation
                .settings
                .read()
                .unwrap()
                .viewport,
            Some((800, 600, 2.0))
        );
        let status = tokio::time::timeout(std::time::Duration::from_secs(1), client.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let status: Value = serde_json::from_str(&status).unwrap();
        assert_eq!(status["viewportWidth"], 800);
        assert_eq!(status["viewportHeight"], 600);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn firefox_window_viewport_survives_late_context_events_and_tab_switches() {
        use super::super::network::{spawn_dispatcher, subscribe_dispatcher};
        use super::super::session::tests::{fake_session_with, Messages};

        let messages = Messages::default();
        let seen = messages.clone();
        let (ff, tx, _) = fake_session_with(move |request| {
            seen.lock().unwrap().push(request.clone());
            match request["method"].as_str() {
                Some("browser.createUserContext") => json!({"userContext": "jar-window"}),
                Some("browsingContext.create") => json!({"context": "ctx-window"}),
                Some("browsingContext.getTree") => json!({"contexts": [
                    {"context": "ctx-active", "userContext": "default"},
                    {"context": "ctx-window", "userContext": "jar-window"},
                ]}),
                _ => json!({}),
            }
        })
        .await;
        let _dispatcher = spawn_dispatcher(
            ff.bidi.clone(),
            ff.net.clone(),
            subscribe_dispatcher(&ff.bidi),
            ff.emulation.clone(),
            ff.permissions.clone(),
        );
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        set_viewport(&json!({"width": 800, "height": 600}), &mut state)
            .await
            .unwrap();
        tabs::window_new(
            &json!({"viewport": {"width": 1000, "height": 700}}),
            &mut state,
        )
        .await
        .unwrap();
        let widths = || {
            messages
                .lock()
                .unwrap()
                .iter()
                .filter(|message| message["method"] == "browsingContext.setViewport")
                .map(|message| {
                    (
                        message["params"]["context"].as_str().unwrap().to_owned(),
                        message["params"]["viewport"]["width"].as_i64().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let before = widths().len();
        tx.send(
            json!({"type":"event", "method":"browsingContext.contextCreated",
            "params":{"context":"ctx-window", "parent":null, "userContext":"jar-window"}}),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while widths().len() == before {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for tab in ["t1", "t2"] {
            tabs::tab_switch(&json!({"tabId": tab}), &mut state)
                .await
                .unwrap();
        }
        assert_eq!(
            &widths()[before..],
            &[
                ("ctx-window".into(), 1000),
                ("ctx-active".into(), 800),
                ("ctx-window".into(), 1000)
            ],
        );
        assert_eq!(state.session_setup.viewport.unwrap().width, 800);

        set_viewport(&json!({"width": 900, "height": 600}), &mut state)
            .await
            .unwrap();
        let ff = state.firefox.as_mut().unwrap();
        assert!(ff
            .emulation
            .settings
            .read()
            .unwrap()
            .window_viewports
            .is_empty());
        assert_eq!(state.session_setup.viewport.unwrap().width, 900);
    }

    #[tokio::test]
    async fn mobile_emulation_is_rejected_without_commands_or_state_changes() {
        let (mut state, calls) = session_state().await;
        let expected_viewport = Some(crate::native::actions::ViewportSetup {
            width: 800,
            height: 600,
            device_scale_factor: 1.5,
            mobile: false,
        });
        state.session_setup.viewport = expected_viewport;
        let ff = state.firefox.as_mut().unwrap();
        ff.emulation.settings.write().unwrap().viewport = Some((800, 600, 1.5));
        ff.emulation.settings.write().unwrap().user_agent = Some("Desktop Agent".to_string());

        let error = set_viewport(
            &json!({"width": 375, "height": 812, "deviceScaleFactor": 3.0, "mobile": true}),
            &mut state,
        )
        .await
        .unwrap_err();
        assert!(error.contains("mobile viewport emulation"));
        assert_eq!(state.session_setup.viewport, expected_viewport);
        assert_eq!(
            state
                .firefox
                .as_ref()
                .unwrap()
                .emulation
                .settings
                .read()
                .unwrap()
                .viewport,
            Some((800, 600, 1.5))
        );

        let error =
            super::super::actions::execute("device", &json!({"device": "iPhone 15"}), &mut state)
                .await
                .unwrap_err();
        assert!(error.contains("set device"));
        assert!(error.contains("set viewport"));
        assert!(error.contains("--user-agent") && error.contains("AGENT_BROWSER_USER_AGENT"));
        assert_eq!(state.session_setup.viewport, expected_viewport);
        assert_eq!(
            state
                .firefox
                .as_ref()
                .unwrap()
                .emulation
                .settings
                .read()
                .unwrap()
                .viewport,
            Some((800, 600, 1.5))
        );
        assert_eq!(
            state
                .firefox
                .as_ref()
                .unwrap()
                .emulation
                .settings
                .read()
                .unwrap()
                .user_agent
                .as_deref(),
            Some("Desktop Agent")
        );
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn timezone_locale_and_offline_are_scoped_to_the_cookie_jar_of_the_active_tab() {
        let (ff, _, _, bidi, _) = fake_session_with_rdp().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        set_timezone(&json!({"timezoneId": "Asia/Tokyo"}), &mut state)
            .await
            .unwrap();
        set_locale(&json!({"locale": "fr-FR"}), &mut state)
            .await
            .unwrap();
        set_offline(&json!({"offline": true}), &mut state)
            .await
            .unwrap();

        let messages = bidi.lock().unwrap().clone();
        let find = |method: &str| {
            messages
                .iter()
                .find(|m| m["method"] == method)
                .unwrap_or_else(|| panic!("no {method} message"))
        };
        assert_eq!(
            find("emulation.setTimezoneOverride")["params"]["timezone"],
            "Asia/Tokyo"
        );
        assert_eq!(
            find("emulation.setTimezoneOverride")["params"]["userContexts"],
            json!(["default"])
        );
        assert_eq!(
            find("emulation.setLocaleOverride")["params"]["locale"],
            "fr-FR"
        );
        assert_eq!(
            find("emulation.setNetworkConditions")["params"]["networkConditions"]["type"],
            "offline"
        );

        set_offline(&json!({"offline": false}), &mut state)
            .await
            .unwrap();
        let updated = bidi.lock().unwrap().clone();
        let last = updated
            .iter()
            .rev()
            .find(|m| m["method"] == "emulation.setNetworkConditions")
            .unwrap();
        assert!(last["params"]["networkConditions"].is_null());
    }

    #[tokio::test]
    async fn unsupported_media_feature_fails_before_touching_the_page() {
        let (mut state, calls) = session_state().await;
        let error = set_media(&json!({"reducedMotion": "reduce"}), &mut state)
            .await
            .unwrap_err();
        assert!(error.contains("prefers-reduced-motion"), "{error}");
        assert!(error.contains("prefers-color-scheme"), "{error}");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn media_updates_the_selected_tab_configuration_actor() {
        let (ff, _, _, _, rdp) = fake_session_with_rdp().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        let result = set_media(
            &json!({
                "colorScheme": "dark",
                "media": "screen",
                "reducedMotion": "no-preference"
            }),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"set": true}));

        let messages = rdp.lock().unwrap().clone();
        let kinds: Vec<&str> = messages
            .iter()
            .map(|m| m["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "getRoot",
                "setIntPref",
                "listTabs",
                "getWatcher",
                "getTargetConfigurationActor",
                "updateConfiguration",
            ]
        );
        assert_eq!(messages[1]["to"], "prefs1");
        assert_eq!(messages[1]["name"], COLOR_SCHEME_PREF);
        assert_eq!(messages[1]["value"], 0);
        assert_eq!(messages[3]["to"], "tab1");
        assert_eq!(messages[4]["to"], "watcher1");
        assert_eq!(messages[5]["to"], "cfg1");
        assert_eq!(
            messages[5]["configuration"],
            json!({"printSimulationEnabled": false})
        );
        assert_eq!(
            state
                .firefox
                .as_ref()
                .unwrap()
                .emulation
                .settings
                .read()
                .unwrap()
                .media
                .as_ref()
                .unwrap()
                .features,
            vec![
                ("prefers-color-scheme".to_string(), "dark".to_string()),
                (
                    "prefers-reduced-motion".to_string(),
                    "no-preference".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn tab_configuration_skips_an_unloaded_first_tab() {
        let (ff, rdp) = crate::native::bidi::session::tests::fake_session_with_rdp_replies(
            |v: &Value| match (v["to"].as_str().unwrap(), v["type"].as_str().unwrap()) {
                ("root", "listTabs") => json!({"from": "root", "tabs": [
                    {"actor": "tab-unloaded", "selected": false, "isZombieTab": true},
                    {"actor": "tab-loaded", "selected": false},
                ]}),
                ("tab-loaded", "getWatcher") => {
                    json!({"from": "tab-loaded", "actor": "watcher1"})
                }
                ("watcher1", "getTargetConfigurationActor") => {
                    json!({"from": "watcher1", "configuration": {"actor": "cfg1"}})
                }
                ("cfg1", "updateConfiguration") => {
                    json!({"from": "cfg1", "configuration": {}})
                }
                _ => json!({"from": v["to"]}),
            },
        )
        .await;
        ff.emulation.settings.write().unwrap().tab_scoped = true;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        set_media(&json!({"colorScheme": "dark"}), &mut state)
            .await
            .unwrap();

        let messages = rdp.lock().unwrap().clone();
        let sent: Vec<(&str, &str)> = messages
            .iter()
            .map(|m| (m["to"].as_str().unwrap(), m["type"].as_str().unwrap()))
            .collect();
        assert_eq!(
            sent,
            vec![
                ("root", "listTabs"),
                ("tab-loaded", "getWatcher"),
                ("watcher1", "getTargetConfigurationActor"),
                ("cfg1", "updateConfiguration"),
            ]
        );
    }

    #[tokio::test]
    async fn firefox_runtime_setup_replays_controls_on_a_fresh_session() {
        use super::super::network::{self, NetContext};
        use std::sync::Arc;

        let (ff, _, _, _, _) = fake_session_with_rdp().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        set_viewport(
            &json!({"width": 800, "height": 600, "deviceScaleFactor": 1.5}),
            &mut state,
        )
        .await
        .unwrap();
        set_user_agent(&json!({"userAgent":"Runtime agent"}), &mut state)
            .await
            .unwrap();
        set_locale(&json!({"locale":"fr-FR"}), &mut state)
            .await
            .unwrap();
        set_timezone(&json!({"timezoneId":"Asia/Tokyo"}), &mut state)
            .await
            .unwrap();
        set_geolocation(
            &json!({"latitude":12.0,"longitude":34.0,"accuracy":2.0}),
            &mut state,
        )
        .await
        .unwrap();
        set_offline(&json!({"offline":true}), &mut state)
            .await
            .unwrap();
        set_media(&json!({"media":"print"}), &mut state)
            .await
            .unwrap();
        network::headers(&json!({"headers":{"x-example":"sample"}}), &mut state)
            .await
            .unwrap();
        let replay_net = |state: &DaemonState| {
            NetContext::from_state(
                state,
                state
                    .session_setup
                    .extra_headers
                    .clone()
                    .unwrap_or_default(),
            )
        };
        let header_net = replay_net(&state);
        assert!(header_net.needs_request_intercept().await);
        assert_eq!(
            header_net.headers.read().await.get("x-example").unwrap(),
            "sample"
        );

        network::credentials(&json!({"username":"user","password":"secret"}), &mut state)
            .await
            .unwrap();
        let credential_net = replay_net(&state);
        assert_eq!(
            *credential_net.headers.read().await,
            HashMap::from([("Authorization".into(), "Basic dXNlcjpzZWNyZXQ=".into())])
        );
        network::headers(&json!({"headers":{}}), &mut state)
            .await
            .unwrap();
        assert!(!replay_net(&state).needs_request_intercept().await);

        let (mut next, _, _, bidi, rdp) = fake_session_with_rdp().await;
        next.emulation = Arc::new(Emulation::new((&state.session_setup).into()));
        apply_to_context(&next, "ctx-active").await.unwrap();
        let bidi = bidi.lock().unwrap();
        for (method, key, value) in [
            (
                "browsingContext.setViewport",
                "devicePixelRatio",
                json!(1.5),
            ),
            (
                "emulation.setUserAgentOverride",
                "userAgent",
                json!("Runtime agent"),
            ),
            ("emulation.setLocaleOverride", "locale", json!("fr-FR")),
            (
                "emulation.setTimezoneOverride",
                "timezone",
                json!("Asia/Tokyo"),
            ),
            (
                "emulation.setGeolocationOverride",
                "coordinates",
                json!({"latitude":12.0,"longitude":34.0,"accuracy":2.0}),
            ),
            (
                "emulation.setNetworkConditions",
                "networkConditions",
                json!({"type":"offline"}),
            ),
        ] {
            assert!(
                bidi.iter()
                    .any(|request| request["method"] == method && request["params"][key] == value),
                "{method}: {bidi:?}"
            );
        }
        assert!(rdp
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["configuration"]["printSimulationEnabled"] == true));
    }

    #[tokio::test]
    async fn a_new_tab_replays_the_session_emulation() {
        let (ff, _, _, bidi, _) = fake_session_with_rdp().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        set_timezone(&json!({"timezoneId": "Asia/Tokyo"}), &mut state)
            .await
            .unwrap();
        tabs::tab_new(&json!({}), &mut state).await.unwrap();

        let messages = bidi.lock().unwrap().clone();
        let timezones: Vec<&Value> = messages
            .iter()
            .filter(|m| m["method"] == "emulation.setTimezoneOverride")
            .collect();
        assert_eq!(timezones.len(), 2);
        assert_eq!(timezones[0]["params"]["userContexts"], json!(["default"]));
        assert_eq!(timezones[1]["params"]["userContexts"], json!(["default"]));
    }
}
