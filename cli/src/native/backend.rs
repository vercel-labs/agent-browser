//! Engine-tagged browser backend.
//!
//! `BrowserBackend` is the single point where the Rust daemon decides whether
//! it is driving Chrome/Lightpanda (CDP) or Camoufox (Playwright sidecar). Every
//! action-layer function that used to accept a bare `&CdpClient` now accepts
//! `&BrowserBackend` and dispatches on the variant; Chrome-only modules assert
//! on the `Cdp` variant at entry and surface an `engine-incompatible` error
//! when pointed at Camoufox.
//!
//! In Unit 1 the Camoufox arm is a stub: `require_cdp` returns a structured
//! `not-yet-implemented` error so agents hit a clean failure mode instead of a
//! panic. Unit 3 fills in the real sidecar client and each action's Camoufox
//! arm is fleshed out in later units.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;

use super::camoufox_client::CamoufoxClient;
use super::cdp::client::CdpClient;
use super::cdp::types::CdpEvent;

/// The engine this daemon session is talking to.
///
/// `Cdp` covers both Chrome and Lightpanda — they share a single CDP
/// transport. `Camoufox` wraps the Python sidecar client.
#[derive(Clone)]
pub enum BrowserBackend {
    Cdp(Arc<CdpClient>),
    Camoufox(Arc<CamoufoxClient>),
}

impl BrowserBackend {
    /// Human-readable engine label, also used as the `"engine"` field in
    /// `--json` output so callers can segment telemetry by backend.
    pub fn engine_name(&self) -> &'static str {
        match self {
            BrowserBackend::Cdp(_) => "cdp",
            BrowserBackend::Camoufox(_) => "camoufox",
        }
    }

    pub fn is_cdp(&self) -> bool {
        matches!(self, BrowserBackend::Cdp(_))
    }

    pub fn is_camoufox(&self) -> bool {
        matches!(self, BrowserBackend::Camoufox(_))
    }

    /// Return the inner CDP client, or a structured `not-yet-implemented`
    /// error when the session is running on Camoufox. Action-layer functions
    /// call this at the top of their body until their Camoufox arm is
    /// implemented; the returned error surfaces to the CLI as a clean failure.
    pub fn require_cdp(&self) -> Result<&Arc<CdpClient>, String> {
        match self {
            BrowserBackend::Cdp(c) => Ok(c),
            BrowserBackend::Camoufox(_) => Err(not_yet_implemented_error(None)),
        }
    }

    /// Chrome-only subsystem entry points (`inspect_server`, `stream::cdp_loop`)
    /// call this instead of `require_cdp` so the error message makes clear that
    /// the feature will not work on Camoufox, rather than "not yet implemented".
    pub fn require_cdp_for(&self, operation: &str) -> Result<&Arc<CdpClient>, String> {
        match self {
            BrowserBackend::Cdp(c) => Ok(c),
            BrowserBackend::Camoufox(_) => Err(engine_incompatible_error(operation)),
        }
    }

    /// Option accessor for non-`Result` contexts (e.g. sync setup paths that
    /// cannot use `?`). Returns `None` on Camoufox.
    pub fn cdp_opt(&self) -> Option<&Arc<CdpClient>> {
        match self {
            BrowserBackend::Cdp(c) => Some(c),
            BrowserBackend::Camoufox(_) => None,
        }
    }

    // ---------------------------------------------------------------------
    // Delegating methods: mirror the handful of `CdpClient` methods that
    // action-layer code calls on the backend. Each arm of the `match` is the
    // "enum arm body" the plan refers to — the Cdp arm forwards to the real
    // CDP client; the Camoufox arm returns `not-yet-implemented` until the
    // corresponding action is wired up in a later unit. Keeping the dispatch
    // at this method level (rather than at each call site) lets us lift
    // function signatures from `&CdpClient` to `&BrowserBackend` without
    // rewriting 130+ action-body lines.
    // ---------------------------------------------------------------------

    pub async fn send_command(
        &self,
        method: &str,
        params: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        match self {
            BrowserBackend::Cdp(c) => c.send_command(method, params, session_id).await,
            BrowserBackend::Camoufox(_) => Err(not_yet_implemented_error(Some(method))),
        }
    }

    pub async fn send_command_typed<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
        session_id: Option<&str>,
    ) -> Result<R, String> {
        match self {
            BrowserBackend::Cdp(c) => c.send_command_typed(method, params, session_id).await,
            BrowserBackend::Camoufox(_) => Err(not_yet_implemented_error(Some(method))),
        }
    }

    pub async fn send_command_no_params(
        &self,
        method: &str,
        session_id: Option<&str>,
    ) -> Result<Value, String> {
        match self {
            BrowserBackend::Cdp(c) => c.send_command_no_params(method, session_id).await,
            BrowserBackend::Camoufox(_) => Err(not_yet_implemented_error(Some(method))),
        }
    }

    /// Subscribe to CDP-shaped events. On Camoufox this surfaces a
    /// `not-yet-implemented` error; callers in the action layer already
    /// propagate with `?` because they return `Result`.
    pub fn subscribe(&self) -> Result<broadcast::Receiver<CdpEvent>, String> {
        match self {
            BrowserBackend::Cdp(c) => Ok(c.subscribe()),
            BrowserBackend::Camoufox(_) => Err(not_yet_implemented_error(Some("subscribe"))),
        }
    }
}

impl std::fmt::Debug for BrowserBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserBackend::Cdp(_) => f.debug_struct("BrowserBackend::Cdp").finish_non_exhaustive(),
            BrowserBackend::Camoufox(_) => f
                .debug_struct("BrowserBackend::Camoufox")
                .finish_non_exhaustive(),
        }
    }
}

/// Structured error returned when an action reaches a Camoufox arm that
/// Unit 3+ has not filled in yet. The JSON shape is stable so downstream
/// tooling (celeria API, dashboards) can pattern-match on `code`.
pub fn not_yet_implemented_error(action: Option<&str>) -> String {
    match action {
        Some(a) => format!(
            "not-yet-implemented: action `{}` is not yet supported on engine=camoufox",
            a
        ),
        None => {
            "not-yet-implemented: this action is not yet supported on engine=camoufox".to_string()
        }
    }
}

/// Structured error for Chrome-only subsystems (raw CDP streaming, DevTools
/// inspect proxy) that will never work on Camoufox. Distinguished from
/// `not-yet-implemented` because callers can fall back to `--engine chrome`
/// but should not wait for a Camoufox implementation that isn't coming.
pub fn engine_incompatible_error(operation: &str) -> String {
    format!(
        "engine-incompatible: `{}` requires engine=chrome (Camoufox does not speak raw CDP)",
        operation
    )
}
