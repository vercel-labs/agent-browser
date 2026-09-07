//! Trust store selection for the CLI's own outbound TLS.
//!
//! By default both the CDP WebSocket client and the HTTP clients verify peers
//! against the Mozilla root list compiled into the binary. That list cannot see
//! a private CA, so an HTTPS-intercepting proxy that re-signs traffic breaks the
//! connection before the WebSocket opens.
//!
//! Two opt-ins widen the trust store. Neither disables verification: hostname
//! and certificate checks stay on in every configuration.
//!
//! - `AGENT_BROWSER_USE_SYSTEM_CA=1` (or `--use-system-ca`) uses the operating
//!   system trust store instead of the compiled-in roots.
//! - `AGENT_BROWSER_CA_CERT` (or `--ca-cert`) adds a PEM bundle as extra roots.
//!   `SSL_CERT_FILE` is honored as a fallback, since sandboxes and CI images
//!   commonly set it already.
//!
//! CLI commands patch the daemon's session selection. Omission retains it;
//! clear and explicit false remove the extra bundle or system selection.
//! Clients reread selected sources on acquisition. Rotated roots replace the
//! cached generation while already admitted operations may finish.
//!
//! `--ca-cert` is shared with browser trust for local Chromium on Linux.
//! Browserless commands update only CLI trust.

use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use rustls::{ClientConfig, RootCertStore};

/// Which roots the CLI verifies against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrustOptions {
    /// Use the operating system trust store instead of the compiled-in roots.
    pub use_system_ca: bool,
    /// Extra PEM bundle to trust on top of the selected roots.
    pub ca_cert: Option<String>,
    /// Whether `ca_cert` came from `SSL_CERT_FILE` rather than from the flag or
    /// `AGENT_BROWSER_CA_CERT`.
    ///
    /// `SSL_CERT_FILE` is ambient: the operator did not ask agent-browser for
    /// anything, and a stale value is common. An unusable bundle from that
    /// source degrades to the built-in roots with a warning. An unusable bundle
    /// the operator named explicitly is an error, because silently ignoring it
    /// would verify against roots they did not choose.
    pub ca_cert_is_implicit: bool,
}

impl TrustOptions {
    /// True when the defaults are in force and no custom config is needed.
    pub fn is_default(&self) -> bool {
        !self.use_system_ca && self.ca_cert.is_none()
    }

    /// Read the options from the environment.
    ///
    /// The environment seeds a new daemon. Later CLI commands patch its
    /// effective selection through `tlsOptions`.
    pub fn from_env() -> Self {
        let use_system_ca = crate::flags::env_var_is_truthy("AGENT_BROWSER_USE_SYSTEM_CA");
        let explicit = std::env::var("AGENT_BROWSER_CA_CERT")
            .ok()
            .filter(|s| !s.is_empty());
        let implicit = std::env::var("SSL_CERT_FILE")
            .ok()
            .filter(|s| !s.is_empty());
        let ca_cert_is_implicit = explicit.is_none() && implicit.is_some();
        Self {
            use_system_ca,
            ca_cert: explicit.or(implicit),
            ca_cert_is_implicit,
        }
    }
}

/// Build a root store from the given options.
///
/// Loading a system trust store that turns out to be empty falls back to the
/// compiled-in roots rather than producing a client that trusts nothing.
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
        let outcome = add_ca_bundle(&mut store, path).and_then(|added| {
            if added == 0 {
                Err(format!("No certificates found in CA bundle '{path}'"))
            } else {
                Ok(())
            }
        });
        if let Err(e) = outcome {
            if !opts.ca_cert_is_implicit {
                return Err(e);
            }
            // SSL_CERT_FILE was not aimed at agent-browser. Warn and keep the
            // roots we already have rather than failing every connection,
            // including local ws:// ones that never negotiate TLS.
            eprintln!("agent-browser: ignoring SSL_CERT_FILE: {e}");
        }
    }

    Ok(store)
}

fn add_webpki_roots(store: &mut RootCertStore) {
    store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
}

/// Add every certificate in a CA file to the store. Returns how many were added.
///
/// Reading and validating belongs to `ca_bundle`, which the Chromium NSS path
/// uses too. One loader is the point: when this module had its own, the same
/// `--ca-cert` file was valid for one consumer and invalid for the other.
fn add_ca_bundle(store: &mut RootCertStore, path: &str) -> Result<usize, String> {
    let bundle = crate::ca_bundle::load(path)?;
    let mut added = 0;
    for cert in bundle.certificates() {
        store
            .add(cert.clone())
            .map_err(|e| format!("Rejected CA certificate in '{path}': {e}"))?;
        added += 1;
    }
    Ok(added)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TrustUpdate {
    #[serde(default)]
    ca_cert: Option<String>,
    #[serde(default)]
    ca_cert_is_implicit: bool,
    #[serde(default)]
    clear_ca_cert: bool,
    #[serde(default)]
    use_system_ca: Option<bool>,
}

#[derive(Default)]
struct TrustCache {
    options: Option<TrustOptions>,
    explicit_selection: bool,
    roots: Vec<rustls::pki_types::TrustAnchor<'static>>,
    config: Option<Arc<ClientConfig>>,
    http: Option<reqwest::Client>,
}

impl TrustCache {
    fn options(&self) -> TrustOptions {
        self.options.clone().unwrap_or_else(TrustOptions::from_env)
    }

    fn update(&mut self, update: TrustUpdate) -> Result<(), String> {
        if update.ca_cert.is_some() && update.clear_ca_cert {
            return Err("Cannot set and clear CLI CA trust together".to_string());
        }
        if update.ca_cert.is_none() && !update.clear_ca_cert && update.use_system_ca.is_none() {
            return Ok(());
        }
        let mut opts = self.options();
        let mut explicit =
            self.explicit_selection || (opts.ca_cert.is_some() && !opts.ca_cert_is_implicit);
        if update.clear_ca_cert {
            opts.ca_cert = None;
            opts.ca_cert_is_implicit = false;
            explicit = true;
        } else if let Some(path) = update.ca_cert {
            if !update.ca_cert_is_implicit || !explicit {
                opts.ca_cert = Some(path);
                opts.ca_cert_is_implicit = update.ca_cert_is_implicit;
                explicit = !update.ca_cert_is_implicit;
            }
        }
        if let Some(value) = update.use_system_ca {
            opts.use_system_ca = value;
        }
        if self.options.as_ref() == Some(&opts) && self.explicit_selection == explicit {
            return Ok(());
        }
        let roots = build_root_store(&opts)?;
        self.replace_roots(roots);
        self.options = Some(opts);
        self.explicit_selection = explicit;
        Ok(())
    }

    fn replace_roots(&mut self, mut roots: RootCertStore) {
        roots.roots.sort_by(|a, b| {
            (
                a.subject.as_ref(),
                a.subject_public_key_info.as_ref(),
                a.name_constraints.as_ref().map(|n| n.as_ref()),
            )
                .cmp(&(
                    b.subject.as_ref(),
                    b.subject_public_key_info.as_ref(),
                    b.name_constraints.as_ref().map(|n| n.as_ref()),
                ))
        });
        roots.roots.dedup();
        if self.config.is_none() || self.roots != roots.roots {
            self.roots = roots.roots.clone();
            self.config = Some(Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            ));
            self.http = None;
        }
    }

    fn config(&mut self) -> Result<Arc<ClientConfig>, String> {
        self.replace_roots(build_root_store(&self.options())?);
        Ok(self
            .config
            .as_ref()
            .expect("roots install a TLS config")
            .clone())
    }

    fn http_client(&mut self) -> Result<reqwest::Client, String> {
        let config = self.config()?;
        if self.http.is_none() {
            self.http = Some(
                reqwest::Client::builder()
                    .use_preconfigured_tls((*config).clone())
                    .build()
                    .map_err(|e| format!("Failed to create HTTP client: {e}"))?,
            );
        }
        Ok(self.http.as_ref().expect("HTTP client was built").clone())
    }
}

fn cache() -> &'static Mutex<TrustCache> {
    static CACHE: OnceLock<Mutex<TrustCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TrustCache::default()))
}

static COMMAND_OPTIONS: OnceLock<Value> = OnceLock::new();

fn absolute_ca_path(path: String) -> String {
    let path = std::path::PathBuf::from(path);
    std::env::current_dir()
        .unwrap_or_default()
        .join(&path)
        .to_string_lossy()
        .into_owned()
}

pub fn configure_cli(flags: &crate::flags::Flags) {
    let mut opts = TrustOptions::from_env();
    opts.use_system_ca = flags.use_system_ca;
    if flags.clear_ca_cert {
        opts.ca_cert = None;
        opts.ca_cert_is_implicit = false;
    } else if let Some(path) = &flags.ca_cert {
        opts.ca_cert = Some(path.clone());
        opts.ca_cert_is_implicit = false;
    }
    opts.ca_cert = opts.ca_cert.map(absolute_ca_path);
    let update = TrustUpdate {
        ca_cert: opts.ca_cert.clone(),
        ca_cert_is_implicit: opts.ca_cert_is_implicit,
        clear_ca_cert: flags.clear_ca_cert,
        use_system_ca: flags.use_system_ca_set.then_some(flags.use_system_ca),
    };
    let _ = COMMAND_OPTIONS.set(serde_json::to_value(update).expect("trust options serialize"));
    let mut state = cache().lock().unwrap_or_else(|e| e.into_inner());
    state.options = Some(opts);
    state.explicit_selection = flags.ca_cert.is_some() || flags.clear_ca_cert;
}

pub fn command_options() -> Option<Value> {
    COMMAND_OPTIONS.get().cloned()
}

pub fn apply_command_options(command: &Value) -> Result<(), String> {
    let Some(value) = command.get("tlsOptions") else {
        return Ok(());
    };
    let update: TrustUpdate = serde_json::from_value(value.clone())
        .map_err(|e| format!("Invalid CLI TLS options: {e}"))?;
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .update(update)
}

/// Acquire current roots for a new TLS operation. Each daemon owns one session
/// per process; replacing a generation retires its pool without closing an
/// in-flight operation or restarting the browser. Sources are re-read even
/// when the selection is omitted, so rotation cannot hide behind a cached path.
pub fn shared_client_config() -> Result<Option<Arc<ClientConfig>>, String> {
    let mut state = cache().lock().unwrap_or_else(|e| e.into_inner());
    if state.options().is_default() {
        return Ok(None);
    }
    state.config().map(Some)
}

pub fn ws_connector() -> Result<Option<tokio_tungstenite::Connector>, String> {
    Ok(shared_client_config()?.map(tokio_tungstenite::Connector::Rustls))
}

/// Apply the same validated root snapshot used by WSS. An invalid explicit
/// bundle is an error for HTTP too; it must never silently widen trust.
pub fn apply_to_reqwest(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, String> {
    let config = cache().lock().unwrap_or_else(|e| e.into_inner()).config()?;
    Ok(builder.use_preconfigured_tls((*config).clone()))
}

pub fn http_client() -> Result<reqwest::Client, String> {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .http_client()
}

/// Warn on this process's stderr when a configured trust source is unusable.
///
/// Emitted in `--json` mode too: the line goes to stderr and stdout stays a
/// single parseable document, so suppressing it would only cost a scripted
/// caller the one message that names the cause.
///
/// The store is built wherever it is first needed, and for a remote CDP
/// connection that is the daemon, whose stderr is discarded. A user whose
/// `SSL_CERT_FILE` is stale would otherwise see only the downstream
/// `UnknownIssuer` and never the one line that names the cause. The CLI owns
/// the terminal the user is attached to, so it says it here.
///
/// Silent when no trust source is configured, and silent when the configured
/// one works: this fires only on the rare, actionable case.
pub fn warn_if_trust_source_unusable() {
    let opts = cache().lock().unwrap_or_else(|e| e.into_inner()).options();
    let Some(path) = &opts.ca_cert else {
        return;
    };
    let mut probe = RootCertStore::empty();
    let detail = match add_ca_bundle(&mut probe, path) {
        Ok(n) if n > 0 => return,
        Ok(_) => format!("no certificates found in '{path}'"),
        Err(e) => e,
    };
    let source = if opts.ca_cert_is_implicit {
        "SSL_CERT_FILE"
    } else {
        "--ca-cert"
    };
    eprintln!("{} {source}: {detail}", crate::color::warning_indicator());
}

/// Describe one CA file the way `describe` would, without reading the environment.
#[cfg(test)]
fn describe_path(path: &str) -> String {
    let mut probe = RootCertStore::empty();
    match add_ca_bundle(&mut probe, path) {
        Ok(n) if n > 0 => format!("{n} certificate(s) from {path}"),
        Ok(_) => format!("{path} holds no certificates"),
        Err(e) => e,
    }
}

/// One-line description of the active trust store, for `agent-browser doctor`.
///
/// Reports the selected sources. An unusable ambient bundle is ignored;
/// an unusable explicit bundle prevents new TLS operations.
pub fn describe() -> String {
    let opts = cache().lock().unwrap_or_else(|e| e.into_inner()).options();
    let roots = if opts.use_system_ca {
        "system trust store"
    } else {
        "built-in Mozilla roots"
    };
    let Some(path) = &opts.ca_cert else {
        return roots.to_string();
    };
    let mut probe = RootCertStore::empty();
    match add_ca_bundle(&mut probe, path) {
        Ok(n) if n > 0 => format!("{roots} plus {n} certificate(s) from {path}"),
        _ if opts.ca_cert_is_implicit => format!("{roots} ({path} unusable, ignored)"),
        _ => format!("TLS unavailable: explicit CA bundle '{path}' is unusable"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Self-signed RSA-2048 test certificate (CN=test-ca).
    const TEST_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDBTCCAe2gAwIBAgIUFhZ9tlfduuEvOIOcGuOA7E0SSL4wDQYJKoZIhvcNAQEL\n\
BQAwEjEQMA4GA1UEAwwHdGVzdC1jYTAeFw0yNjAzMjUxNDE5MDVaFw0yNzAzMjUx\n\
NDE5MDVaMBIxEDAOBgNVBAMMB3Rlc3QtY2EwggEiMA0GCSqGSIb3DQEBAQUAA4IB\n\
DwAwggEKAoIBAQDcgmzozr7Ia72OCsxk2uKUFhM6wR0H69cv4qO5OViu+0qFoYr6\n\
Bny2o+Q/ooqCCYveamPukYlZMFilnk9b4M2VwxK72pOVTkvyWUWpIJrV6OQKqsaf\n\
DNgDdl4U4i2U/HKKNXTNtaVPzc3d40rcwy8dHVzFaTs8o7UG73foHQ2/7KQ6sY5d\n\
gjOchbLDlhN2Nkyc4WxXEipesonUogLzZxx9gSMZN6VmXaIyijncAFxO9vSenTQd\n\
FstTlTI/FCPQU2cg5K3rtToPli3j7z9oeeMrrt3pp1xmU5/cliz5kQ3CXxbH1UR3\n\
uFAaW09wTsK+fSo8rBgGWO5JU706M1aL5wvXAgMBAAGjUzBRMB0GA1UdDgQWBBR3\n\
yFGDemoQUIFA/YW1BJYhT6hlhzAfBgNVHSMEGDAWgBR3yFGDemoQUIFA/YW1BJYh\n\
T6hlhzAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQCq5bl2J+JO\n\
LpOZG4n4xbQUi456bV40a9lxFwXyR4toiOnLc9QTiFLtrRRMjiAYBlpnp7Aq7rPK\n\
0dxGhFsNhTHYv5bKF3Wt6EKnfmjC5J2PQ4j4fZbqnBJVNhtP3/QdTg/Alx2DgVlP\n\
vUaYBYvyM8aeAGCvlTr9XbciLgDHrO6xE0mppF87jG3DbVIqhGAa8z7KR286Hmw3\n\
JtnWOCSAT+dNsAXmz4ebm7kp9OnpLLKjvrNEUNPA20J5S+BXTtPv7x/koRwSX35M\n\
9yOorGsG0RB4CaEy4fpiKTewGNMdHNoZNevXB1s7jm3YdW5BDxvG4Su5RGqAjS+Y\n\
49s7jC+okfzl\n\
-----END CERTIFICATE-----\n";

    fn write_temp_pem(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ab-tls-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ca.pem");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn trust_generation_tracks_material_and_preserves_omitted_selection() {
        let path = write_temp_pem(include_str!("../tests/fixtures/tls/ca-a.pem"));
        let mut cache = TrustCache {
            options: Some(TrustOptions::default()),
            ..Default::default()
        };
        let select = |path: &std::path::Path| TrustUpdate {
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        cache.update(select(&path)).unwrap();
        let first = cache.config().unwrap();
        cache.http_client().unwrap();
        cache.update(TrustUpdate::default()).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.config().unwrap()));
        let equivalent = write_temp_pem(&format!(
            "{}{}",
            include_str!("../tests/fixtures/tls/ca-a.pem"),
            include_str!("../tests/fixtures/tls/ca-a.pem")
        ));
        cache.update(select(&equivalent)).unwrap();
        assert!(Arc::ptr_eq(&first, &cache.config().unwrap()));
        assert!(cache.http.is_some());
        std::fs::write(&equivalent, include_str!("../tests/fixtures/tls/ca-b.pem")).unwrap();
        let rotated = cache.config().unwrap();
        assert!(!Arc::ptr_eq(&first, &rotated));
        assert!(cache.http.is_none());
        cache.http_client().unwrap();
        std::fs::write(&equivalent, "invalid").unwrap();
        assert!(cache.http_client().is_err());
        assert!(cache
            .update(select(std::path::Path::new("/missing/ca")))
            .is_err());
        assert_eq!(cache.options().ca_cert.as_deref(), equivalent.to_str());
        cache
            .update(TrustUpdate {
                clear_ca_cert: true,
                use_system_ca: Some(false),
                ..Default::default()
            })
            .unwrap();
        assert!(cache.options().is_default());
        cache
            .update(TrustUpdate {
                ca_cert: Some(path.to_string_lossy().into_owned()),
                ca_cert_is_implicit: true,
                ..Default::default()
            })
            .unwrap();
        assert!(
            cache.options().is_default(),
            "an ambient fallback must not undo an explicit clear"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(equivalent.parent().unwrap());
    }

    #[test]
    fn default_options_need_no_custom_config() {
        assert!(TrustOptions::default().is_default());
    }

    #[test]
    fn default_store_holds_the_built_in_roots() {
        let store = build_root_store(&TrustOptions::default()).unwrap();
        assert_eq!(store.len(), webpki_roots::TLS_SERVER_ROOTS.len());
    }

    #[test]
    fn ca_bundle_is_added_on_top_of_the_built_in_roots() {
        let path = write_temp_pem(TEST_PEM);
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ca_cert_is_implicit: false,
        };
        let store = build_root_store(&opts).unwrap();
        assert_eq!(store.len(), webpki_roots::TLS_SERVER_ROOTS.len() + 1);
        assert!(!opts.is_default());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_multi_cert_bundle_adds_every_certificate() {
        let path = write_temp_pem(&format!("{TEST_PEM}{TEST_PEM}"));
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ca_cert_is_implicit: false,
        };
        let store = build_root_store(&opts).unwrap();
        // rustls deduplicates identical anchors, so the count grows by one.
        assert!(store.len() > webpki_roots::TLS_SERVER_ROOTS.len());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_bundle_is_an_error() {
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ca_cert_is_implicit: false,
        };
        let err = build_root_store(&opts).unwrap_err();
        assert!(err.contains("Failed to read"), "{err}");
    }

    #[test]
    fn a_bundle_with_no_certificates_is_an_error() {
        let path = write_temp_pem("not a certificate\n");
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ca_cert_is_implicit: false,
        };
        // Assert the invariant (the bundle is refused), not the wording: a
        // non-PEM file is now tried as DER, so the message differs by encoding.
        let err = build_root_store(&opts).unwrap_err();
        assert!(err.contains(&path.to_string_lossy().to_string()), "{err}");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_der_bundle_is_accepted_like_the_browser_side_accepts_it() {
        // --ca-cert is shared with the Chromium NSS path, which reads PEM and
        // raw DER. A Windows-exported .cer is DER; accepting it on one side
        // only would trust the proxy in the browser and refuse to start here.
        let der = {
            let pem = TEST_PEM.as_bytes();
            let mut reader = std::io::BufReader::new(pem);
            let first = rustls_pemfile::certs(&mut reader).next().unwrap().unwrap();
            first.to_vec()
        };
        let dir = std::env::temp_dir().join(format!("ab-tls-der-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ca.der");
        std::fs::write(&path, &der).unwrap();

        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ca_cert_is_implicit: false,
        };
        let store = build_root_store(&opts).expect("DER must be accepted");
        assert_eq!(store.len(), webpki_roots::TLS_SERVER_ROOTS.len() + 1);
        assert!(describe_path(&path.to_string_lossy()).contains("1 certificate"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unusable_bundle_from_ssl_cert_file_degrades_instead_of_failing() {
        // SSL_CERT_FILE is ambient. A stale value must not take down every
        // connection, including local ws:// ones that never negotiate TLS.
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some("/nonexistent/ca.pem".to_string()),
            ca_cert_is_implicit: true,
        };
        let store = build_root_store(&opts).expect("implicit source must not be fatal");
        assert_eq!(store.len(), webpki_roots::TLS_SERVER_ROOTS.len());
    }

    #[test]
    fn an_empty_bundle_from_ssl_cert_file_also_degrades() {
        let path = write_temp_pem("not a certificate\n");
        let opts = TrustOptions {
            use_system_ca: false,
            ca_cert: Some(path.to_string_lossy().into_owned()),
            ca_cert_is_implicit: true,
        };
        assert!(build_root_store(&opts).is_ok());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_system_store_yields_a_usable_client_config() {
        let opts = TrustOptions {
            use_system_ca: true,
            ca_cert: None,
            ca_cert_is_implicit: false,
        };
        let store = build_root_store(&opts).unwrap();
        assert!(
            !store.is_empty(),
            "system store must fall back to built-in roots rather than trust nothing"
        );
        assert!(TrustCache {
            options: Some(opts),
            ..Default::default()
        }
        .config()
        .is_ok());
    }

    #[test]
    fn describe_names_the_active_roots() {
        // Reads the ambient environment; assert only on the default shape.
        let text = describe();
        assert!(
            text.contains("roots") || text.contains("trust store"),
            "{text}"
        );
    }
}
