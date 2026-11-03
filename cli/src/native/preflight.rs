//! Single pre-execution gate for daemon commands.
//!
//! Every command that is about to drive the browser passes through [`run`]
//! before the action executes, no matter which launch path, engine, or
//! attachment produced that browser. Checks are listed once in [`CHECKS`], so
//! adding a rule means adding a variant and its implementation instead of
//! editing every command, engine, or connection path.
//!
//! Three call sites feed this gate, and each is the moment a command has a
//! browser it could act on:
//!
//! * `actions::auto_launch_inner`, immediately after an implicit launch
//!   adopts the browser and before pages are restored or init scripts run.
//! * `actions::establish_launch`, immediately after an explicit `launch`
//!   adopts a locally launched browser, for the same ordering reason.
//! * `actions::execute_command`, before the dispatcher runs the action, which
//!   covers attached browsers (`--cdp`, `--auto-connect`) and every command
//!   that reuses a browser an earlier command started.
//!
//! Only a blocking verdict is cached, keyed by the CDP product string. A pass
//! is never cached: a cached `Ok` would also freeze the fail-open result of a
//! network failure and would hide advisories published later in the session.

use super::actions::DaemonState;

/// Everything the checks need, resolved once per command.
pub struct Target {
    pub engine: String,
    /// Chrome/Chromium reports `Chrome/<version>` here; other engines report
    /// their own product string, and an empty value means the browser did not
    /// answer.
    pub product: String,
}

/// Checks run in registration order; the first failure stops the command.
const CHECKS: &[Check] = &[Check::OutdatedBrowser];

#[derive(Clone, Copy)]
enum Check {
    /// Refuse a build that is behind stable and missing documented fixes.
    OutdatedBrowser,
}

impl Check {
    async fn run(self, target: &Target) -> Result<(), String> {
        match self {
            Check::OutdatedBrowser => {
                crate::browser_security::enforce(&target.engine, &target.product, false).await
            }
        }
    }
}

/// Commands that must keep working on a browser the checks reject, because
/// they are how a caller gets out of a stuck session.
fn always_allowed(action: &str) -> bool {
    matches!(action, "close" | "dialog" | "confirm" | "deny")
}

/// Run the gate for `action` against the browser this command would drive.
///
/// Unknown browsers, unsupported engines, provider sessions, and callers that
/// opted out all pass, so the gate never fails a command it cannot judge or
/// the caller has explicitly allowed.
pub async fn run(action: &str, state: &mut DaemonState) -> Result<(), String> {
    if always_allowed(action) || state.allow_outdated_browser || state.is_provider_session() {
        return Ok(());
    }
    let Some(manager) = state.browser.as_ref() else {
        return Ok(());
    };
    let product = browser_product(manager).await;
    if let Some((blocked_product, message)) = &state.browser_block {
        if *blocked_product == product {
            return Err(message.clone());
        }
    }

    let target = Target {
        engine: state.engine.clone(),
        product: product.clone(),
    };
    match run_checks(&target).await {
        Ok(()) => Ok(()),
        Err(message) => {
            state.browser_block = Some((product, message.clone()));
            Err(message)
        }
    }
}

async fn run_checks(target: &Target) -> Result<(), String> {
    for check in CHECKS {
        check.run(target).await?;
    }
    Ok(())
}

/// The CDP product string of the connected browser, or empty when it does not
/// answer. This is the same `Browser.getVersion` roundtrip the daemon already
/// makes for liveness, so the gate adds no new failure mode.
async fn browser_product(manager: &super::browser::BrowserManager) -> String {
    manager
        .client
        .send_command_no_params("Browser.getVersion", None)
        .await
        .ok()
        .and_then(|value| {
            value
                .get("product")
                .and_then(|product| product.as_str())
                .map(String::from)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_hatches_are_always_allowed() {
        for action in ["close", "dialog", "confirm", "deny"] {
            assert!(always_allowed(action), "{action} must stay usable");
        }
        for action in ["open", "navigate", "click", "snapshot", "launch", "read"] {
            assert!(!always_allowed(action), "{action} must be gated");
        }
    }

    #[test]
    fn checks_are_registered_in_a_single_place() {
        // Guards the registry shape: adding a check appends here and nowhere
        // else. If this fails, move the new rule into `Check`.
        assert_eq!(CHECKS.len(), 1);
    }
}
