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
//! `--ca-cert` is shared with the browser-side trust flag, so one path covers
//! both Chromium and the CLI.

use std::sync::Arc;
use std::sync::OnceLock;

use rustls::pki_types::CertificateDer;
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
    /// The daemon runs as a separate process, so the environment is the only
    /// channel that carries these across the spawn. `connection::apply_daemon_env`
    /// forwards both variables.
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
        let outcome = add_pem_bundle(&mut store, path).and_then(|added| {
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

/// Add every certificate in a PEM file to the store. Returns how many were added.
fn add_pem_bundle(store: &mut RootCertStore, path: &str) -> Result<usize, String> {
    let data =
        std::fs::read(path).map_err(|e| format!("Failed to read CA certificate '{path}': {e}"))?;
    let mut reader = std::io::BufReader::new(data.as_slice());
    let mut added = 0;
    for cert in rustls_pemfile::certs(&mut reader) {
        let cert: CertificateDer<'_> =
            cert.map_err(|e| format!("Failed to parse CA certificate '{path}': {e}"))?;
        store
            .add(cert)
            .map_err(|e| format!("Rejected CA certificate in '{path}': {e}"))?;
        added += 1;
    }
    Ok(added)
}

fn client_config(opts: &TrustOptions) -> Result<Arc<ClientConfig>, String> {
    let store = build_root_store(opts)?;
    let config = ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Process-wide client config, built once from the environment.
///
/// `Ok(None)` means the defaults are in force and callers should use the
/// library default connector.
pub fn shared_client_config() -> Result<Option<Arc<ClientConfig>>, String> {
    static CACHED: OnceLock<Result<Option<Arc<ClientConfig>>, String>> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let opts = TrustOptions::from_env();
            if opts.is_default() {
                return Ok(None);
            }
            client_config(&opts).map(Some)
        })
        .clone()
}

/// TLS connector for `tokio-tungstenite`, or `None` to use its default.
pub fn ws_connector() -> Result<Option<tokio_tungstenite::Connector>, String> {
    Ok(shared_client_config()?.map(tokio_tungstenite::Connector::Rustls))
}

/// Apply the configured roots to a `reqwest` client builder.
pub fn apply_to_reqwest(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    let opts = TrustOptions::from_env();
    if opts.is_default() {
        return builder;
    }

    let mut builder = builder;

    // The system store replaces the built-in roots; a CA bundle adds to
    // whichever set is active.
    if opts.use_system_ca {
        let native = load_native_der();
        if native.is_empty() {
            eprintln!("agent-browser: system trust store unavailable, using built-in roots");
        } else {
            builder = builder.tls_built_in_root_certs(false);
            for der in native {
                if let Ok(c) = reqwest::Certificate::from_der(&der) {
                    builder = builder.add_root_certificate(c);
                }
            }
        }
    }

    if let Some(path) = &opts.ca_cert {
        match std::fs::read(path) {
            Ok(pem) => match reqwest::Certificate::from_pem_bundle(&pem) {
                Ok(certs) => {
                    for c in certs {
                        builder = builder.add_root_certificate(c);
                    }
                }
                Err(e) => eprintln!("agent-browser: failed to parse CA bundle '{path}': {e}"),
            },
            Err(e) => eprintln!("agent-browser: failed to read CA bundle '{path}': {e}"),
        }
    }

    builder
}

/// A shared `reqwest` client that honors the configured trust store.
///
/// Built once and cloned, which is cheap: `reqwest::Client` is a handle around
/// a shared pool. Constructing one per call would rebuild the TLS config and
/// the connection pool on every request, and CDP discovery calls this twice per
/// iteration of a retry loop.
///
/// Falls back to a default client if the builder cannot be constructed, so a
/// bad CA path degrades to the previous behavior instead of killing the command.
pub fn http_client() -> reqwest::Client {
    static CACHED: OnceLock<reqwest::Client> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            apply_to_reqwest(reqwest::Client::builder())
                .build()
                .unwrap_or_default()
        })
        .clone()
}

fn load_native_der() -> Vec<Vec<u8>> {
    rustls_native_certs::load_native_certs()
        .certs
        .into_iter()
        .map(|c| c.as_ref().to_vec())
        .collect()
}

/// One-line description of the active trust store, for `agent-browser doctor`.
///
/// Reports what is actually in force. A CA bundle that could not be read is
/// named as ignored rather than listed as trusted, since the whole point of the
/// line is to tell someone debugging a proxy which roots they really have.
pub fn describe() -> String {
    let opts = TrustOptions::from_env();
    let roots = if opts.use_system_ca {
        "system trust store"
    } else {
        "built-in Mozilla roots"
    };
    let Some(path) = &opts.ca_cert else {
        return roots.to_string();
    };
    let mut probe = RootCertStore::empty();
    match add_pem_bundle(&mut probe, path) {
        Ok(n) if n > 0 => format!("{roots} plus {n} certificate(s) from {path}"),
        Ok(_) => format!("{roots} ({path} holds no certificates, ignored)"),
        Err(_) => format!("{roots} ({path} unreadable, ignored)"),
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
        let err = build_root_store(&opts).unwrap_err();
        assert!(err.contains("No certificates found"), "{err}");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
        assert!(client_config(&opts).is_ok());
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
