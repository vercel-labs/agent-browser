//! Single pre-execution gate for daemon commands.
//!
//! Every command that is about to drive the browser passes through [`run_with_tls`]
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
//! Only a blocking verdict is cached, keyed by the CDP product string and
//! Browser.getVersion user agent. A pass is never cached: a cached `Ok` would
//! also freeze the fail-open result of a network failure and would hide
//! advisories published later in the session.

use super::actions::DaemonState;

/// Everything the checks need, resolved once per command.
pub struct Target {
    pub engine: String,
    /// Chrome/Chromium reports `Chrome/<version>` here; other engines report
    /// their own product string, and an empty value means the browser did not
    /// answer.
    pub product: String,
    pub user_agent: String,
}

/// Checks run in registration order; the first failure stops the command.
const CHECKS: &[Check] = &[Check::OutdatedBrowser];

#[derive(Clone, Copy)]
enum Check {
    /// Refuse a build that is behind stable and missing documented fixes.
    OutdatedBrowser,
}

impl Check {
    async fn run(
        self,
        target: &Target,
        trust: &crate::tls::TrustOptions,
    ) -> Result<Option<String>, crate::browser_security::EnforcementError> {
        match self {
            Check::OutdatedBrowser => {
                crate::browser_security::enforce_with_tls(
                    &target.engine,
                    &target.product,
                    &target.user_agent,
                    false,
                    trust,
                )
                .await
            }
        }
    }
}

/// Parse the trust options carried by one daemon request.
pub fn trust_from_command(cmd: &serde_json::Value) -> Result<crate::tls::TrustOptions, String> {
    match cmd.get("securityTls") {
        Some(value) if value.is_object() => serde_json::from_value(value.clone())
            .map_err(|_| "Invalid browser security TLS options".to_string()),
        Some(_) => Err("Invalid browser security TLS options".to_string()),
        None => Ok(crate::tls::TrustOptions::default()),
    }
}

/// Remove request-local trust details before retaining or broadcasting a command.
pub fn redact_command_trust(cmd: &mut serde_json::Value) {
    if let Some(object) = cmd.as_object_mut() {
        object.remove("securityTls");
        object.remove("tls");
    }
}

/// Check a product before adoption and return any fail-open warning.
pub async fn check_product(
    engine: &str,
    product: &str,
    user_agent: &str,
    trust: &crate::tls::TrustOptions,
) -> Result<Option<String>, crate::browser_security::EnforcementError> {
    let target = Target {
        engine: engine.to_string(),
        product: product.to_string(),
        user_agent: user_agent.to_string(),
    };
    run_checks(&target, trust).await
}

/// Commands that must keep working on a browser the checks reject, because
/// they are how a caller gets out of a stuck session.
fn always_allowed(action: &str) -> bool {
    matches!(action, "close" | "dialog" | "confirm" | "deny")
}

/// Run the gate against the connected browser using this request's trust options.
pub async fn run_with_tls(
    action: &str,
    state: &mut DaemonState,
    trust: &crate::tls::TrustOptions,
) -> Result<Option<String>, String> {
    if always_allowed(action) || state.allow_outdated_browser || state.is_provider_session() {
        return Ok(None);
    }
    let Some(manager) = state.browser.as_ref() else {
        return Ok(None);
    };
    let (product, user_agent) = browser_version(manager).await;
    run_product(state, &product, &user_agent, trust).await
}

/// Gate a product before adoption using this request's trust options.
pub async fn run_product(
    state: &mut DaemonState,
    product: &str,
    user_agent: &str,
    trust: &crate::tls::TrustOptions,
) -> Result<Option<String>, String> {
    if state.allow_outdated_browser || state.is_provider_session() {
        return Ok(None);
    }
    let shared_memo = shares_blocking_memo(trust);
    if shared_memo {
        if let Some((blocked_product, blocked_user_agent, message)) = &state.browser_block {
            if blocked_product == product && blocked_user_agent == user_agent {
                return Err(message.clone());
            }
        }
    }

    match check_product(&state.engine, product, user_agent, trust).await {
        Ok(warning) => Ok(warning),
        Err(crate::browser_security::EnforcementError::Outdated(message)) => {
            if shared_memo {
                state.browser_block =
                    Some((product.to_string(), user_agent.to_string(), message.clone()));
            }
            Err(message)
        }
        Err(error) => Err(error.into_message()),
    }
}

/// A blocking verdict is reusable only between requests that share the default trust.
fn shares_blocking_memo(trust: &crate::tls::TrustOptions) -> bool {
    trust.is_default()
}

async fn run_checks(
    target: &Target,
    trust: &crate::tls::TrustOptions,
) -> Result<Option<String>, crate::browser_security::EnforcementError> {
    let mut warnings = Vec::new();
    for check in CHECKS {
        if let Some(warning) = check.run(target, trust).await? {
            warnings.push(warning);
        }
    }
    Ok((!warnings.is_empty()).then(|| warnings.join("\n")))
}

/// The CDP product and user agent of the connected browser, or empty when it does not answer.
async fn browser_version(manager: &super::browser::BrowserManager) -> (String, String) {
    let version = manager
        .client
        .send_command_no_params("Browser.getVersion", None)
        .await
        .unwrap_or_default();
    let product = version
        .get("product")
        .and_then(|product| product.as_str())
        .unwrap_or_default()
        .to_string();
    let user_agent = version
        .get("userAgent")
        .and_then(|user_agent| user_agent.as_str())
        .unwrap_or_default()
        .to_string();
    (product, user_agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_trust_is_not_inherited_and_malformed_fields_are_rejected() {
        let first = serde_json::json!({
            "securityTls": {
                "useSystemCa": true,
                "caCert": "/tmp/custom-ca.pem",
                "caCertIsImplicit": false
            }
        });
        let trust = trust_from_command(&first).unwrap();
        assert!(trust.use_system_ca);
        assert_eq!(trust.ca_cert.as_deref(), Some("/tmp/custom-ca.pem"));
        let mut redacted = first.clone();
        redacted["tls"] = first["securityTls"].clone();
        redact_command_trust(&mut redacted);
        assert!(redacted.get("securityTls").is_none());
        assert!(redacted.get("tls").is_none());
        assert_eq!(
            trust_from_command(&serde_json::json!({})).unwrap(),
            crate::tls::TrustOptions::default()
        );
        for invalid in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"useSystemCa": "yes"}),
        ] {
            assert!(trust_from_command(&serde_json::json!({"securityTls": invalid})).is_err());
        }
    }

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
    fn blocking_verdicts_are_shared_only_under_default_trust() {
        assert!(shares_blocking_memo(&crate::tls::TrustOptions::default()));
        for trust in [
            crate::tls::TrustOptions {
                use_system_ca: true,
                ..Default::default()
            },
            crate::tls::TrustOptions {
                ca_cert: Some("/tmp/custom-ca.pem".to_string()),
                ..Default::default()
            },
            crate::tls::TrustOptions {
                ca_cert: Some("/tmp/stale-ca.pem".to_string()),
                ca_cert_is_implicit: true,
                ..Default::default()
            },
        ] {
            assert!(!shares_blocking_memo(&trust));
        }
    }

    #[test]
    fn checks_are_registered_in_a_single_place() {
        assert!(CHECKS
            .iter()
            .any(|check| matches!(check, Check::OutdatedBrowser)));
    }
}
