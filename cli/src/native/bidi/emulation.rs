use serde_json::{json, Map, Value};

use super::actions::session;
use super::forms::require;
use super::session::FirefoxSession;
use crate::native::actions::{device_preset, DaemonState, EmulatedMedia};

pub(crate) type Command = (&'static str, Value);

/// Emulation applied to the active tab and replayed onto tabs the daemon opens.
#[derive(Debug, Default, Clone)]
pub struct Settings {
    pub viewport: Option<(i64, i64, f64)>,
    pub user_agent: Option<String>,
    pub locale: Option<String>,
    pub timezone: Option<String>,
    pub geolocation: Option<(f64, f64, Option<f64>)>,
    pub offline: Option<bool>,
    pub media: Option<EmulatedMedia>,
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

/// Maps a permission and its origin to `permissions.setPermission`.
pub(crate) fn permission(name: &str, state: &str, origin: &str) -> Command {
    (
        "permissions.setPermission",
        json!({
            "descriptor": { "name": name },
            "state": state,
            "origin": origin,
        }),
    )
}

/// Maps a device preset to viewport and user agent commands.
pub(crate) fn device(name: &str) -> Result<Vec<Command>, String> {
    let preset = device_preset(name)?;
    Ok(vec![
        viewport(
            preset.width as i64,
            preset.height as i64,
            preset.device_scale_factor,
        ),
        user_agent(preset.user_agent),
    ])
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
                config.insert(
                    "colorSchemeSimulation".to_string(),
                    Value::String(value.clone()),
                );
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

/// Sends an emulation command scoped to one browsing context.
async fn send_scoped(
    ff: &FirefoxSession,
    context: &str,
    (method, mut params): Command,
) -> Result<(), String> {
    if method == "browsingContext.setViewport" {
        params["context"] = json!(context);
    } else {
        params["contexts"] = json!([context]);
    }
    ff.bidi.send(method, params).await.map(|_| ())
}

/// Applies color scheme and print media to the selected tab through its DevTools configuration actor.
pub(crate) async fn update_tab_configuration(
    ff: &FirefoxSession,
    context: &str,
    config: &Value,
) -> Result<(), String> {
    ff.bidi
        .send("browsingContext.activate", json!({"context": context}))
        .await?;
    let rdp = &ff.rdp;
    let tabs = rdp.request("root", "listTabs", json!({})).await?;
    let tab = tabs["tabs"]
        .as_array()
        .and_then(|tabs| {
            tabs.iter()
                .find(|tab| tab["selected"].as_bool() == Some(true))
                .or_else(|| tabs.first())
        })
        .ok_or("Firefox reported no tabs for target configuration")?;
    let descriptor = tab["actor"]
        .as_str()
        .ok_or("Firefox reported a tab without a descriptor actor")?;
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

/// Replays every stored setting onto `context`, used for tabs the daemon creates.
pub async fn apply_to_context(ff: &FirefoxSession, context: &str) -> Result<(), String> {
    let settings = &ff.emulation;
    if let Some((width, height, ratio)) = settings.viewport {
        send_scoped(ff, context, viewport(width, height, ratio)).await?;
    }
    if let Some(ref user_agent) = settings.user_agent {
        send_scoped(ff, context, self::user_agent(user_agent)).await?;
    }
    if let Some(ref locale) = settings.locale {
        send_scoped(ff, context, self::locale(locale)).await?;
    }
    if let Some(ref timezone) = settings.timezone {
        send_scoped(ff, context, self::timezone(timezone)).await?;
    }
    if let Some((latitude, longitude, accuracy)) = settings.geolocation {
        send_scoped(ff, context, geolocation(latitude, longitude, accuracy)).await?;
    }
    if let Some(offline) = settings.offline {
        send_scoped(ff, context, self::offline(offline)).await?;
    }
    if let Some(ref media) = settings.media {
        update_tab_configuration(ff, context, &media_config(media)?).await?;
    }
    Ok(())
}

/// Sets the viewport and returns Chrome's `viewport` result shape.
pub async fn set_viewport(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let width = cmd.get("width").and_then(Value::as_i64).unwrap_or(1280);
    let height = cmd.get("height").and_then(Value::as_i64).unwrap_or(720);
    let scale = cmd
        .get("deviceScaleFactor")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    let mobile = cmd.get("mobile").and_then(Value::as_bool).unwrap_or(false);
    let context = session(state)?.top_context.clone();
    send_scoped(session(state)?, &context, viewport(width, height, scale)).await?;
    session(state)?.emulation.viewport = Some((width, height, scale));
    state.viewport = Some((width as i32, height as i32, scale, mobile));
    if let Some(ref server) = state.stream_server {
        server.set_viewport(width as u32, height as u32).await;
    }
    Ok(json!({
        "width": width,
        "height": height,
        "deviceScaleFactor": scale,
        "mobile": mobile,
    }))
}

/// Applies a device preset and returns Chrome's `device` result shape.
pub async fn set_device(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .or_else(|| cmd.get("device"))
        .and_then(Value::as_str)
        .ok_or("Missing 'name' parameter")?;
    let preset = device_preset(name)?;
    let commands = device(name)?;
    let context = session(state)?.top_context.clone();
    for command in commands {
        send_scoped(session(state)?, &context, command).await?;
    }
    {
        let ff = session(state)?;
        ff.emulation.viewport = Some((
            preset.width as i64,
            preset.height as i64,
            preset.device_scale_factor,
        ));
        ff.emulation.user_agent = Some(preset.user_agent.to_string());
    }
    state.viewport = Some((
        preset.width,
        preset.height,
        preset.device_scale_factor,
        preset.mobile,
    ));
    if let Some(ref server) = state.stream_server {
        server
            .set_viewport(preset.width as u32, preset.height as u32)
            .await;
    }
    Ok(json!({
        "device": name,
        "width": preset.width,
        "height": preset.height,
        "deviceScaleFactor": preset.device_scale_factor,
        "mobile": preset.mobile,
    }))
}

/// Overrides the user agent for the active tab and any tab opened later.
pub async fn set_user_agent(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let user_agent = require(cmd, "userAgent")?;
    let context = session(state)?.top_context.clone();
    send_scoped(session(state)?, &context, self::user_agent(user_agent)).await?;
    session(state)?.emulation.user_agent = Some(user_agent.to_string());
    Ok(json!({ "userAgent": user_agent }))
}

/// Overrides the locale for the active tab and any tab opened later.
pub async fn set_locale(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let locale = require(cmd, "locale")?;
    let context = session(state)?.top_context.clone();
    send_scoped(session(state)?, &context, self::locale(locale)).await?;
    session(state)?.emulation.locale = Some(locale.to_string());
    Ok(json!({ "locale": locale }))
}

/// Overrides the timezone for the active tab and any tab opened later.
pub async fn set_timezone(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timezone = cmd
        .get("timezoneId")
        .or_else(|| cmd.get("timezone"))
        .and_then(Value::as_str)
        .ok_or("Missing 'timezoneId' parameter")?;
    let context = session(state)?.top_context.clone();
    send_scoped(session(state)?, &context, self::timezone(timezone)).await?;
    session(state)?.emulation.timezone = Some(timezone.to_string());
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
    let context = session(state)?.top_context.clone();
    send_scoped(
        session(state)?,
        &context,
        geolocation(latitude, longitude, accuracy),
    )
    .await?;
    session(state)?.emulation.geolocation = Some((latitude, longitude, accuracy));
    Ok(json!({ "latitude": latitude, "longitude": longitude }))
}

/// Grants each permission for the active page's origin.
pub async fn set_permissions(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let permissions: Vec<String> = cmd
        .get("permissions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if permissions.is_empty() {
        return Ok(json!({ "granted": permissions }));
    }
    let url = session(state)?.url().await?;
    let origin = page_origin(&url)?;
    for name in &permissions {
        session(state)?
            .bidi
            .send(
                "permissions.setPermission",
                permission(name, "granted", &origin).1,
            )
            .await?;
    }
    Ok(json!({ "granted": permissions }))
}

fn page_origin(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|_| {
        "Cannot set permissions without a page origin; navigate to a page first".to_string()
    })?;
    let origin = parsed.origin();
    if origin.is_tuple() {
        Ok(origin.ascii_serialization())
    } else {
        Err("Cannot set permissions without a page origin; navigate to a page first".to_string())
    }
}

/// Toggles offline mode for the active tab and any tab opened later.
pub async fn set_offline(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let offline = cmd.get("offline").and_then(Value::as_bool).unwrap_or(true);
    let context = session(state)?.top_context.clone();
    send_scoped(session(state)?, &context, self::offline(offline)).await?;
    session(state)?.emulation.offline = Some(offline);
    Ok(json!({ "offline": offline }))
}

/// Applies color scheme and print media to the active tab.
pub async fn set_media(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let media = parse_media(cmd);
    let config = media_config(&media)?;
    {
        let ff = session(state)?;
        let context = ff.top_context.clone();
        update_tab_configuration(ff, &context, &config).await?;
    }
    session(state)?.emulation.media = Some(media);
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
    use crate::native::bidi::session::tests::{fake_session, fake_session_with_rdp};
    use crate::native::bidi::tabs;

    fn media_with_feature(name: &str, value: &str) -> EmulatedMedia {
        EmulatedMedia {
            media: None,
            features: vec![(name.to_string(), value.to_string())],
        }
    }

    #[test]
    fn reduced_motion_lists_supported_features() {
        let error = validate_media(&media_with_feature("prefers-reduced-motion", "reduce"))
            .expect_err("reduced motion is not supported");
        assert!(error.contains("prefers-reduced-motion"));
        assert!(error.contains("prefers-color-scheme"));
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
            state.firefox.as_ref().unwrap().emulation.viewport,
            Some((800, 600, 2.0))
        );
    }

    #[tokio::test]
    async fn permissions_without_a_page_origin_fail_before_granting() {
        let (mut state, calls) = session_state().await;
        let error = set_permissions(&json!({"permissions": ["geolocation"]}), &mut state)
            .await
            .unwrap_err();
        assert!(error.contains("page origin"), "{error}");
        assert!(!calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "permissions.setPermission"));
    }

    #[tokio::test]
    async fn unsupported_media_feature_fails_before_touching_the_page() {
        let (mut state, calls) = session_state().await;
        let error = set_media(&json!({"reducedMotion": "reduce"}), &mut state)
            .await
            .unwrap_err();
        assert!(error.contains("prefers-reduced-motion"), "{error}");
        assert!(calls.lock().unwrap().is_empty());
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
        assert_eq!(timezones[0]["params"]["contexts"], json!(["ctx-active"]));
        assert_eq!(timezones[1]["params"]["contexts"], json!(["ctx-new"]));
    }
}
