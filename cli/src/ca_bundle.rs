//! Loading the CA certificates named by `--ca-cert`.
//!
//! One loader, so every consumer of the flag accepts and refuses exactly the
//! same files. On Linux, each certificate is imported into the isolated NSS
//! database used by the locally launched Chromium process.
//!
//! Two encodings are accepted, the two an operator is likely to have on hand:
//! a PEM bundle of one or more certificates, and a single raw DER certificate
//! (what Windows exports as `.cer`). PEM is tried first because
//! `rustls_pemfile` skips any preamble, so an `openssl x509 -text` dump with
//! its human-readable header still loads.

use rustls::pki_types::CertificateDer;
use rustls::RootCertStore;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct CaBundle {
    certs: Vec<CertificateDer<'static>>,
    digest: [u8; 32],
}

impl CaBundle {
    pub fn certificates(&self) -> &[CertificateDer<'static>] {
        &self.certs
    }

    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
}

/// Read and validate every certificate in a CA file.
///
/// Nothing is returned that a trust-anchor conversion rejected. A file that yields no
/// usable certificate is an error; a partially valid PEM bundle is an error
/// too, so two consumers can never end up trusting different subsets of it.
pub fn load(path: &str) -> Result<CaBundle, String> {
    let data =
        std::fs::read(path).map_err(|e| format!("Failed to read CA certificate '{path}': {e}"))?;

    let mut reader = std::io::BufReader::new(data.as_slice());
    let mut certs: Vec<CertificateDer<'static>> = Vec::new();
    for cert in rustls_pemfile::certs(&mut reader) {
        certs.push(cert.map_err(|e| format!("Failed to parse CA certificate '{path}': {e}"))?);
    }

    // No PEM block at all: the file may be a single DER certificate. Only a
    // parser gets to decide that.
    if certs.is_empty() {
        let cert = CertificateDer::from(data);
        validate(&cert).map_err(|e| {
            format!("'{path}' holds no PEM certificate and is not a DER certificate: {e}")
        })?;
        return Ok(bundle(vec![cert]));
    }

    for cert in &certs {
        validate(cert).map_err(|e| format!("Rejected CA certificate in '{path}': {e}"))?;
    }

    Ok(bundle(certs))
}

fn bundle(mut certs: Vec<CertificateDer<'static>>) -> CaBundle {
    certs.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
    certs.dedup_by(|left, right| left.as_ref() == right.as_ref());
    let mut hasher = Sha256::new();
    for cert in &certs {
        hasher.update((cert.len() as u64).to_be_bytes());
        hasher.update(cert.as_ref());
    }
    CaBundle {
        certs,
        digest: hasher.finalize().into(),
    }
}

#[cfg(test)]
pub(crate) fn test_bundle(identity: &[u8]) -> CaBundle {
    let mut hasher = Sha256::new();
    hasher.update(identity);
    CaBundle {
        certs: Vec::new(),
        digest: hasher.finalize().into(),
    }
}

/// True when a trust-anchor conversion accepts these bytes as an X.509
/// certificate. This is the same check `RootCertStore::add` performs.
///
/// Structure only: it does not require `basicConstraints CA:TRUE` and does not
/// look at validity dates, matching what the trust store itself accepts. The
/// point is that the two consumers of the flag agree, not that either becomes
/// stricter than the platform.
fn validate(cert: &CertificateDer<'_>) -> Result<(), String> {
    let mut probe = RootCertStore::empty();
    probe.add(cert.clone()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, body: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ab-ca-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Self-signed RSA-2048 test certificate (CN=test-ca).
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

    fn pem() -> &'static str {
        TEST_PEM
    }

    #[test]
    fn a_pem_bundle_loads_every_certificate() {
        let path = write("ca.pem", format!("{}{}", pem(), pem()).as_bytes());
        let certs = load(path.to_str().unwrap()).unwrap();
        assert_eq!(certs.certificates().len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bundle_identity_ignores_duplicate_certificates() {
        let single_path = write("single.pem", pem().as_bytes());
        let duplicate_path = write("duplicate.pem", format!("{}{}", pem(), pem()).as_bytes());

        let single = load(single_path.to_str().unwrap()).unwrap();
        let duplicate = load(duplicate_path.to_str().unwrap()).unwrap();

        assert_eq!(single.digest(), duplicate.digest());
        let _ = std::fs::remove_dir_all(single_path.parent().unwrap());
        let _ = std::fs::remove_dir_all(duplicate_path.parent().unwrap());
    }

    #[test]
    fn a_pem_bundle_behind_a_text_preamble_still_loads() {
        // openssl prints subject/issuer above the block; rustls_pemfile skips
        // it. A prefix check on "-----BEGIN" would send this to the DER branch.
        let body = format!("subject=CN=test-ca\nissuer=CN=test-ca\n{}", pem());
        let path = write("ca.pem", body.as_bytes());
        assert_eq!(
            load(path.to_str().unwrap()).unwrap().certificates().len(),
            1
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_der_certificate_loads() {
        let der = {
            let mut reader = std::io::BufReader::new(pem().as_bytes());
            let first = rustls_pemfile::certs(&mut reader).next().unwrap().unwrap();
            first.to_vec()
        };
        let path = write("ca.der", &der);
        assert_eq!(
            load(path.to_str().unwrap()).unwrap().certificates().len(),
            1
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bytes_that_are_not_a_certificate_are_refused() {
        // A DER SEQUENCE holding six trivial elements walks like a certificate
        // to a positional parser and is not one. Nothing may reach a consumer
        // on the strength of its shape alone.
        let inner: Vec<u8> = [
            vec![0x02, 0x01, 0x01],
            vec![0x02, 0x01, 0x02],
            vec![0x02, 0x01, 0x03],
            vec![0x02, 0x01, 0x04],
            vec![0x02, 0x01, 0x05],
            vec![0x04, 0x05, b'f', b'a', b'k', b'e', b'!'],
        ]
        .concat();
        let tbs = [vec![0x30, inner.len() as u8], inner].concat();
        let cert = [vec![0x30, tbs.len() as u8], tbs].concat();

        let path = write("fake.der", &cert);
        let err = load(path.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a DER certificate"), "{err}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_empty_file_is_refused() {
        let path = write("empty.pem", b"");
        assert!(load(path.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_file_names_itself() {
        let err = load("/nonexistent/ca.pem").unwrap_err();
        assert!(err.contains("/nonexistent/ca.pem"), "{err}");
    }
}
