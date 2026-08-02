use std::env;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::EvaluateParams;

pub const FEATURE_NAME: &str = "agent-cursor";
pub const INSTALL_SCRIPT: &str = include_str!("agent_cursor.js");

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorShape {
    #[default]
    Arrow,
    Ring,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorGlow {
    None,
    #[default]
    Soft,
    Strong,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorMotion {
    #[default]
    Smooth,
    Direct,
}

fn default_accent() -> String {
    "#7c3aed".to_string()
}

fn default_scale() -> f64 {
    1.0
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct CursorTheme {
    pub shape: CursorShape,
    pub accent: String,
    pub glow: CursorGlow,
    pub scale: f64,
    pub motion: CursorMotion,
}

impl Default for CursorTheme {
    fn default() -> Self {
        Self {
            shape: CursorShape::default(),
            accent: default_accent(),
            glow: CursorGlow::default(),
            scale: default_scale(),
            motion: CursorMotion::default(),
        }
    }
}

impl CursorTheme {
    fn validate(self) -> Result<Self, String> {
        let valid_accent = self.accent.len() == 7
            && self.accent.starts_with('#')
            && self.accent[1..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit());
        if !valid_accent {
            return Err("accent must be a six-digit hex color such as #7c3aed".to_string());
        }
        if !(0.75..=1.5).contains(&self.scale) {
            return Err("scale must be between 0.75 and 1.5".to_string());
        }
        Ok(self)
    }

    fn from_env() -> Self {
        let Some(raw) = env::var("AGENT_BROWSER_CURSOR_THEME").ok() else {
            return Self::default();
        };
        let parsed = serde_json::from_str::<Self>(&raw)
            .map_err(|error| error.to_string())
            .and_then(Self::validate);
        match parsed {
            Ok(theme) => theme,
            Err(error) => {
                eprintln!(
                    "warning: invalid AGENT_BROWSER_CURSOR_THEME; using the default: {error}"
                );
                Self::default()
            }
        }
    }
}

fn feature_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|raw| {
        raw.split([',', '\n'])
            .map(str::trim)
            .any(|feature| feature == FEATURE_NAME)
    })
}

#[derive(Debug, Default)]
pub struct CursorState {
    enabled: bool,
    visible_session_id: Option<String>,
    theme: CursorTheme,
}

impl CursorState {
    pub fn from_env() -> Self {
        Self {
            enabled: feature_enabled(env::var("AGENT_BROWSER_ENABLE").ok().as_deref()),
            visible_session_id: None,
            theme: CursorTheme::from_env(),
        }
    }

    pub fn configure(&mut self, features: &[String]) {
        self.enabled = features.iter().any(|feature| feature == FEATURE_NAME);
        self.visible_session_id = None;
    }

    pub fn reset(&mut self) {
        self.enabled = false;
        self.visible_session_id = None;
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn theme_json(&self) -> String {
        serde_json::to_string(&self.theme).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn forget_session(&mut self, session_id: &str) {
        if self.visible_session_id.as_deref() == Some(session_id) {
            self.visible_session_id = None;
        }
    }
}

async fn evaluate(
    client: &CdpClient,
    session_id: &str,
    expression: String,
    await_promise: bool,
) -> Result<(), String> {
    tokio::time::timeout(
        Duration::from_millis(900),
        client.send_command_typed::<_, Value>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression,
                return_by_value: Some(true),
                await_promise: Some(await_promise),
            },
            Some(session_id),
        ),
    )
    .await
    .map_err(|_| "Agent cursor evaluation timed out".to_string())??;
    Ok(())
}

async fn evaluate_no_wait(client: &CdpClient, session_id: &str, expression: String) {
    let params = serde_json::to_value(EvaluateParams {
        expression,
        return_by_value: Some(false),
        await_promise: Some(false),
    })
    .ok();
    let _ = client
        .send_command_no_wait("Runtime.evaluate", params, Some(session_id))
        .await;
}

async fn hide(client: &CdpClient, session_id: &str) {
    let expression = "globalThis.__agentBrowserCursor?.hide()".to_string();
    evaluate_no_wait(client, session_id, expression).await;
}

/// Transfer the one visible cursor between page or OOPIF sessions.
async fn claim_session(client: &CdpClient, state: &mut CursorState, input_session_id: &str) {
    if state.visible_session_id.as_deref() == Some(input_session_id) {
        return;
    }
    if let Some(previous_session_id) = state.visible_session_id.take() {
        hide(client, &previous_session_id).await;
    }
    state.visible_session_id = Some(input_session_id.to_string());
}

/// Move the visual agent cursor in the same page target that receives input.
///
/// OOPIF input coordinates are local to the iframe target, not the outer page.
/// Rendering in `input_session_id` keeps the overlay aligned without trying to
/// duplicate Chromium's frame-transform machinery. Only the previously visible
/// target is hidden when ownership changes.
pub async fn move_to(
    client: &CdpClient,
    state: &mut CursorState,
    input_session_id: &str,
    x: f64,
    y: f64,
) {
    if !state.enabled() {
        return;
    }

    claim_session(client, state, input_session_id).await;

    let x = serde_json::to_string(&x).unwrap_or_else(|_| "0".to_string());
    let y = serde_json::to_string(&y).unwrap_or_else(|_| "0".to_string());
    let theme = state.theme_json();
    let expression = format!(
        "{INSTALL_SCRIPT}; globalThis.__agentBrowserCursor.configure({theme}); globalThis.__agentBrowserCursor.moveTo({x}, {y})"
    );
    let _ = evaluate(client, input_session_id, expression, true).await;
}

pub async fn correct_to(client: &CdpClient, state: &CursorState, session_id: &str, x: f64, y: f64) {
    if !state.enabled() {
        return;
    }
    let x = serde_json::to_string(&x).unwrap_or_else(|_| "0".to_string());
    let y = serde_json::to_string(&y).unwrap_or_else(|_| "0".to_string());
    let theme = state.theme_json();
    let expression = format!(
        "{INSTALL_SCRIPT}; globalThis.__agentBrowserCursor.configure({theme}); globalThis.__agentBrowserCursor.moveTo({x}, {y}, true)"
    );
    let _ = evaluate(client, session_id, expression, true).await;
}

/// Enqueue a raw press visualization before its input event without waiting
/// for a Runtime response. CDP preserves command order on the flat session.
pub async fn press_at(
    client: &CdpClient,
    state: &mut CursorState,
    session_id: &str,
    x: f64,
    y: f64,
) {
    if !state.enabled() {
        return;
    }
    claim_session(client, state, session_id).await;
    let x = serde_json::to_string(&x).unwrap_or_else(|_| "0".to_string());
    let y = serde_json::to_string(&y).unwrap_or_else(|_| "0".to_string());
    let theme = state.theme_json();
    let expression = format!(
        "{INSTALL_SCRIPT}; globalThis.__agentBrowserCursor.configure({theme}); globalThis.__agentBrowserCursor.placeAt({x}, {y}); globalThis.__agentBrowserCursor.pulse()"
    );
    evaluate_no_wait(client, session_id, expression).await;
}

pub async fn place_at(
    client: &CdpClient,
    state: &mut CursorState,
    session_id: &str,
    x: f64,
    y: f64,
) {
    if !state.enabled() {
        return;
    }
    claim_session(client, state, session_id).await;
    let x = serde_json::to_string(&x).unwrap_or_else(|_| "0".to_string());
    let y = serde_json::to_string(&y).unwrap_or_else(|_| "0".to_string());
    let theme = state.theme_json();
    let expression = format!(
        "{INSTALL_SCRIPT}; globalThis.__agentBrowserCursor.configure({theme}); globalThis.__agentBrowserCursor.placeAt({x}, {y})"
    );
    evaluate_no_wait(client, session_id, expression).await;
}

pub async fn pulse(client: &CdpClient, state: &CursorState, session_id: &str) {
    if !state.enabled() {
        return;
    }
    let theme = state.theme_json();
    let expression = format!(
        "{INSTALL_SCRIPT}; globalThis.__agentBrowserCursor.configure({theme}); globalThis.__agentBrowserCursor.pulse()"
    );
    evaluate_no_wait(client, session_id, expression).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_exact_feature_in_comma_or_newline_lists() {
        assert!(feature_enabled(Some("react-devtools,agent-cursor")));
        assert!(feature_enabled(Some("react-devtools\n agent-cursor ")));
        assert!(!feature_enabled(Some("agent-cursors")));
        assert!(!feature_enabled(Some("cursor")));
        assert!(!feature_enabled(None));
    }

    #[test]
    fn daemon_state_uses_the_resolved_launch_features() {
        let mut state = CursorState::default();
        state.configure(&["agent-cursor".to_string()]);
        assert!(state.enabled());

        state.configure(&[]);
        assert!(!state.enabled());
        assert!(state.visible_session_id.is_none());
    }

    #[test]
    fn cursor_theme_accepts_partial_typed_overrides() {
        let theme: CursorTheme = serde_json::from_str(
            r##"{"shape":"ring","accent":"#8c264c","glow":"strong","scale":1.2}"##,
        )
        .unwrap();
        let theme = theme.validate().unwrap();
        assert!(matches!(theme.shape, CursorShape::Ring));
        assert_eq!(theme.accent, "#8c264c");
        assert!(matches!(theme.glow, CursorGlow::Strong));
        assert!(matches!(theme.motion, CursorMotion::Smooth));
        assert_eq!(theme.scale, 1.2);
    }

    #[test]
    fn cursor_theme_rejects_unbounded_values() {
        let invalid_color: CursorTheme = serde_json::from_str(r##"{"accent":"red"}"##).unwrap();
        assert!(invalid_color.validate().is_err());

        let invalid_scale: CursorTheme = serde_json::from_str(r#"{"scale":2}"#).unwrap();
        assert!(invalid_scale.validate().is_err());

        assert!(serde_json::from_str::<CursorTheme>(r#"{"shape":"custom-svg"}"#).is_err());
        assert!(serde_json::from_str::<CursorTheme>(r#"{"css":"display:none"}"#).is_err());
    }

    #[test]
    fn overlay_is_non_interactive_and_reduced_motion_aware() {
        assert!(INSTALL_SCRIPT.contains("pointerEvents: \"none\""));
        assert!(INSTALL_SCRIPT.contains("prefers-reduced-motion: reduce"));
        assert!(INSTALL_SCRIPT.contains("requestAnimationFrame"));
        assert!(INSTALL_SCRIPT.contains("showPopover"));
        assert!(INSTALL_SCRIPT.contains("configurable: false"));
        assert!(INSTALL_SCRIPT.contains("style.setProperty(property, value, \"important\")"));
    }
}
