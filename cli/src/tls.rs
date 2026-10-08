//! Trust roots for the CLI's own HTTPS requests.
//!
//! By default the CLI verifies peers against the Mozilla roots compiled into
//! the binary. A private CA installed by a sandbox, MDM profile, or enterprise
//! image is invisible to that list, so an HTTPS-intercepting proxy breaks
//! `install`, `upgrade`, `doctor` probes, and `read` with `UnknownIssuer`.
//!
//! Two opt-ins widen the trust store. Neither disables verification:
//!
//! - `--use-system-ca` uses the operating system trust store instead of the
//!   compiled-in roots.
//! - `--ca-cert <path>` adds a PEM bundle or DER certificate on top of the
//!   selected roots. `SSL_CERT_FILE` is honored as an ambient fallback.
//!
//! Session commands such as `read` send the resolved options with every
//! request, so the daemon holds no trust state. An explicit selection made on
//! the command line is saved beside the session until `close`, which keeps it
//! in effect when the daemon restarts for an unrelated config change.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rustls::{ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};

use crate::flags::Flags;

/// Which roots the CLI verifies against.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrustOptions {
    /// Use the operating system trust store instead of the compiled-in roots.
    pub use_system_ca: bool,
    /// Extra CA file to trust on top of the selected roots.
    pub ca_cert: Option<String>,
    /// Whether `ca_cert` came from `SSL_CERT_FILE`. That variable was not set
    /// for agent-browser and is often stale, so an unusable file is ignored
    /// with a warning instead of failing the request.
    pub ca_cert_is_implicit: bool,
}

impl TrustOptions {
    pub fn is_default(&self) -> bool {
        !self.use_system_ca && self.ca_cert.is_none()
    }
}

/// Explicit command-line selection saved for a session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SessionTrust {
    ca_cert: Option<String>,
    clear_ca_cert: bool,
    use_system_ca: Option<bool>,
}

impl SessionTrust {
    fn has_ca_selection(&self) -> bool {
        self.ca_cert.is_some() || self.clear_ca_cert
    }
}

fn session_trust_path(session: &str) -> PathBuf {
    crate::connection::get_socket_dir().join(format!("{session}.trust.json"))
}

fn read_session_trust(session: &str) -> SessionTrust {
    std::fs::read(session_trust_path(session))
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

/// Forget a session's saved selection. Called when the session is closed.
pub fn clear_session(session: &str) {
    let _ = std::fs::remove_file(session_trust_path(session));
}

/// Symlinks stay unresolved, so rotating the link target takes effect.
fn absolute(path: &str) -> String {
    std::env::current_dir()
        .unwrap_or_default()
        .join(Path::new(path))
        .to_string_lossy()
        .into_owned()
}

fn ambient_ca_cert() -> Option<String> {
    std::env::var("SSL_CERT_FILE")
        .ok()
        .filter(|s| !s.is_empty())
}

fn resolve(flags: &Flags, saved: &SessionTrust) -> (TrustOptions, SessionTrust) {
    let mut next = saved.clone();

    let (ca_cert, clear_ca_cert) = if flags.cli_ca_cert {
        next.ca_cert = flags.ca_cert.as_deref().map(absolute);
        next.clear_ca_cert = flags.clear_ca_cert;
        (next.ca_cert.clone(), next.clear_ca_cert)
    } else if saved.has_ca_selection() {
        (saved.ca_cert.clone(), saved.clear_ca_cert)
    } else {
        (flags.ca_cert.as_deref().map(absolute), flags.clear_ca_cert)
    };

    let use_system_ca = if flags.cli_use_system_ca {
        next.use_system_ca = Some(flags.use_system_ca);
        flags.use_system_ca
    } else {
        saved.use_system_ca.unwrap_or(flags.use_system_ca)
    };

    let options = match ca_cert {
        Some(path) => TrustOptions {
            use_system_ca,
            ca_cert: Some(path),
            ca_cert_is_implicit: false,
        },
        None if clear_ca_cert => TrustOptions {
            use_system_ca,
            ..TrustOptions::default()
        },
        None => TrustOptions {
            use_system_ca,
            ca_cert: ambient_ca_cert().map(|path| absolute(&path)),
            ca_cert_is_implicit: true,
        },
    };
    (options, next)
}

/// Resolve trust for a command sent to `session`, saving any selection made
/// on this command line. A `--ca-cert` file that does not load is used for
/// this request only, so its error is reported without replacing the saved
/// selection.
pub fn session_options(flags: &Flags, session: &str) -> TrustOptions {
    let saved = read_session_trust(session);
    let (options, next) = resolve(flags, &saved);
    let loads = next
        .ca_cert
        .as_deref()
        .is_none_or(|path| next.ca_cert == saved.ca_cert || crate::ca_bundle::load(path).is_ok());
    if next != saved && loads {
        if let Ok(data) = serde_json::to_vec(&next) {
            let _ = std::fs::create_dir_all(crate::connection::get_socket_dir());
            let _ = std::fs::write(session_trust_path(session), data);
        }
    }
    options
}

static PROCESS_OPTIONS: OnceLock<TrustOptions> = OnceLock::new();

/// Set trust for requests made by this process outside a session, such as
/// `install`, `upgrade`, and `doctor`.
pub fn configure_process(flags: &Flags) {
    let (options, _) = resolve(flags, &SessionTrust::default());
    let _ = PROCESS_OPTIONS.set(options);
}

pub fn process_options() -> TrustOptions {
    PROCESS_OPTIONS.get().cloned().unwrap_or_default()
}

/// Build a root store for the given options.
///
/// A system trust store that loads empty falls back to the compiled-in roots
/// instead of producing a client that trusts nothing.
pub fn build_root_store(opts: &TrustOptions) -> Result<RootCertStore, String> {
    let mut store = RootCertStore::empty();

    if opts.use_system_ca {
        let result = rustls_native_certs::load_native_certs();
        for cert in result.certs {
            let _ = store.add(cert);
        }
        if store.is_empty() {
            let detail = result
                .errors
                .first()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no certificates found".to_string());
            eprintln!(
                "agent-browser: system trust store unavailable ({detail}), using built-in roots"
            );
            add_webpki_roots(&mut store);
        }
    } else {
        add_webpki_roots(&mut store);
    }

    if let Some(path) = &opts.ca_cert {
        if let Err(e) = add_ca_bundle(&mut store, path) {
            if !opts.ca_cert_is_implicit {
                return Err(e);
            }
            eprintln!("agent-browser: ignoring SSL_CERT_FILE: {e}");
        }
    }

    Ok(store)
}

fn add_webpki_roots(store: &mut RootCertStore) {
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
}

/// Parsing belongs to `ca_bundle`, which the Chromium NSS path uses too, so
/// both consumers of `--ca-cert` accept exactly the same files.
fn add_ca_bundle(store: &mut RootCertStore, path: &str) -> Result<usize, String> {
    let bundle = crate::ca_bundle::load(path)?;
    for cert in bundle.certificates() {
        store
            .add(cert.clone())
            .map_err(|e| format!("Rejected CA certificate in '{path}': {e}"))?;
    }
    Ok(bundle.certificates().len())
}

/// Apply the selected roots to an HTTP client builder. Default options leave
/// the builder unchanged.
pub fn apply_to_reqwest(
    builder: reqwest::ClientBuilder,
    opts: &TrustOptions,
) -> Result<reqwest::ClientBuilder, String> {
    if opts.is_default() {
        return Ok(builder);
    }
    let config = ClientConfig::builder()
        .with_root_certificates(build_root_store(opts)?)
        .with_no_client_auth();
    Ok(builder.use_preconfigured_tls(config))
}

/// One-line description of the selected roots, for `agent-browser doctor`.
pub fn describe(opts: &TrustOptions) -> String {
    let roots = if opts.use_system_ca {
        "system trust store"
    } else {
        "built-in Mozilla roots"
    };
    let Some(path) = &opts.ca_cert else {
        return roots.to_string();
    };
    match crate::ca_bundle::load(path) {
        Ok(bundle) => format!(
            "{roots} plus {} certificate(s) from {path}",
            bundle.certificates().len()
        ),
        Err(_) if opts.ca_cert_is_implicit => format!("{roots} ({path} unusable, ignored)"),
        Err(e) => format!("unavailable: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> Flags {
        crate::flags::parse_flags(&[])
    }

    #[test]
    fn cli_selection_is_saved_and_reused() {
        let mut first = flags();
        first.ca_cert = Some("/tmp/ca.pem".to_string());
        first.cli_ca_cert = true;
        let (opts, saved) = resolve(&first, &SessionTrust::default());
        assert_eq!(opts.ca_cert.as_deref(), Some("/tmp/ca.pem"));
        assert!(!opts.ca_cert_is_implicit);

        let (reused, unchanged) = resolve(&flags(), &saved);
        assert_eq!(reused.ca_cert.as_deref(), Some("/tmp/ca.pem"));
        assert_eq!(unchanged, saved);
    }

    #[test]
    fn cli_clear_overrides_saved_selection() {
        let saved = SessionTrust {
            ca_cert: Some("/tmp/ca.pem".to_string()),
            ..SessionTrust::default()
        };
        let mut clear = flags();
        clear.clear_ca_cert = true;
        clear.cli_ca_cert = true;
        let (opts, next) = resolve(&clear, &saved);
        assert_eq!(opts.ca_cert, None);
        assert!(next.clear_ca_cert);
        assert_eq!(next.ca_cert, None);
    }

    #[test]
    fn saved_system_ca_survives_until_turned_off() {
        let mut on = flags();
        on.use_system_ca = true;
        on.cli_use_system_ca = true;
        let (_, saved) = resolve(&on, &SessionTrust::default());
        assert!(resolve(&flags(), &saved).0.use_system_ca);

        let mut off = flags();
        off.use_system_ca = false;
        off.cli_use_system_ca = true;
        let (opts, next) = resolve(&off, &saved);
        assert!(!opts.use_system_ca);
        assert_eq!(next.use_system_ca, Some(false));
    }

    #[test]
    fn options_round_trip_through_command_json() {
        let opts = TrustOptions {
            use_system_ca: true,
            ca_cert: Some("/tmp/ca.pem".to_string()),
            ca_cert_is_implicit: false,
        };
        let value = serde_json::to_value(&opts).unwrap();
        assert_eq!(value["useSystemCa"], true);
        assert_eq!(value["caCert"], "/tmp/ca.pem");
        assert_eq!(serde_json::from_value::<TrustOptions>(value).unwrap(), opts);
    }

    #[test]
    fn unusable_explicit_bundle_is_an_error() {
        let opts = TrustOptions {
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ..TrustOptions::default()
        };
        assert!(build_root_store(&opts).is_err());
    }

    #[test]
    fn unusable_ambient_bundle_keeps_built_in_roots() {
        let opts = TrustOptions {
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ca_cert_is_implicit: true,
            ..TrustOptions::default()
        };
        assert!(!build_root_store(&opts).unwrap().is_empty());
    }
}
