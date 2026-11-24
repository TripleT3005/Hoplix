use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::Verifier;

use crate::connection::HostKeyTrust;
use crate::{Error, ErrorKind, Result};

// Types

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Accept,
    AcceptAndRemember,
    Untrusted,
    Changed,
}

#[derive(Debug)]
pub(crate) struct CertificateCheck {
    platform: Verifier,
    host: String,
    port: u16,
    store: PathBuf,
    trust: Option<HostKeyTrust>,
    rejection: Arc<Mutex<Option<Error>>>,
}

// Fingerprints

pub fn certificate_fingerprint(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let hex: Vec<String> = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    format!("SHA256:{}", hex.join(":"))
}

// Configuration

pub(crate) fn client_config(
    host: &str,
    port: u16,
    store: &Path,
    trust: Option<HostKeyTrust>,
    rejection: Arc<Mutex<Option<Error>>>,
) -> Result<Arc<ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let platform = Verifier::new(Arc::clone(&provider)).map_err(tls_error)?;
    let check = CertificateCheck {
        platform,
        host: host.to_string(),
        port,
        store: store.to_path_buf(),
        trust,
        rejection,
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(tls_error)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(check))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn tls_error(error: rustls::Error) -> Error {
    Error::new(ErrorKind::Other, error.to_string())
}

// Decisions

fn decide(
    fingerprint: &str,
    platform_trusted: bool,
    stored: Option<&str>,
    trust: Option<&HostKeyTrust>,
) -> Decision {
    if platform_trusted || stored == Some(fingerprint) {
        return Decision::Accept;
    }
    if stored.is_some() {
        return Decision::Changed;
    }
    match trust {
        Some(trust) if trust.fingerprint == fingerprint && trust.remember => {
            Decision::AcceptAndRemember
        }
        Some(trust) if trust.fingerprint == fingerprint => Decision::Accept,
        _ => Decision::Untrusted,
    }
}

impl CertificateCheck {
    fn evaluate(&self, end_entity: &CertificateDer<'_>, platform_trusted: bool) -> Result<()> {
        let fingerprint = certificate_fingerprint(end_entity.as_ref());
        let stored = stored_fingerprint(&self.store, &self.host, self.port);
        match decide(
            &fingerprint,
            platform_trusted,
            stored.as_deref(),
            self.trust.as_ref(),
        ) {
            Decision::Accept => Ok(()),
            Decision::AcceptAndRemember => {
                remember(&self.store, &self.host, self.port, &fingerprint)
                    .map_err(|error| Error::from_io(&error, &self.store.to_string_lossy()))
            }
            Decision::Untrusted => Err(Error::new(ErrorKind::UntrustedCertificate, fingerprint)),
            Decision::Changed => Err(Error::new(ErrorKind::CertificateChanged, fingerprint)),
        }
    }
}

impl ServerCertVerifier for CertificateCheck {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let platform_trusted = self
            .platform
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .is_ok();
        match self.evaluate(end_entity, platform_trusted) {
            Ok(()) => Ok(ServerCertVerified::assertion()),
            Err(error) => {
                if let Ok(mut rejection) = self.rejection.lock() {
                    *rejection = Some(error);
                }
                Err(rustls::Error::General("untrusted certificate".to_string()))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.platform
            .verify_tls12_signature(message, certificate, signature)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.platform
            .verify_tls13_signature(message, certificate, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.platform.supported_verify_schemes()
    }
}

// Storage

fn store_key(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{}]:{port}", host.to_ascii_lowercase())
    } else {
        format!("{}:{port}", host.to_ascii_lowercase())
    }
}

fn stored_fingerprint(store: &Path, host: &str, port: u16) -> Option<String> {
    let key = store_key(host, port);
    fs::read_to_string(store).ok()?.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(&key)))
        .then(|| parts.next().map(str::to_string))
        .flatten()
    })
}

fn remember(store: &Path, host: &str, port: u16, fingerprint: &str) -> std::io::Result<()> {
    if let Some(parent) = store.parent() {
        fs::create_dir_all(parent)?;
    }
    let key = store_key(host, port);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store)?;
    writeln!(file, "{key} {fingerprint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trust(fingerprint: &str, remember: bool) -> HostKeyTrust {
        HostKeyTrust {
            fingerprint: fingerprint.to_string(),
            remember,
        }
    }

    // Fingerprints

    #[test]
    fn fingerprints_are_colon_separated_sha256() {
        let fingerprint = certificate_fingerprint(b"abc");
        assert!(fingerprint.starts_with("SHA256:BA:78:16:BF:"));
        assert!(fingerprint.ends_with(":F2:00:15:AD"));
        assert_eq!(fingerprint.len(), "SHA256:".len() + 32 * 3 - 1);
    }

    // Decisions

    #[test]
    fn valid_and_known_certificates_are_accepted() {
        assert_eq!(decide("A", true, None, None), Decision::Accept);
        assert_eq!(decide("A", true, Some("B"), None), Decision::Accept);
        assert_eq!(decide("A", false, Some("A"), None), Decision::Accept);
    }

    #[test]
    fn unknown_certificates_need_trust() {
        assert_eq!(decide("A", false, None, None), Decision::Untrusted);
        assert_eq!(
            decide("A", false, None, Some(&trust("B", true))),
            Decision::Untrusted
        );
        assert_eq!(
            decide("A", false, None, Some(&trust("A", false))),
            Decision::Accept
        );
        assert_eq!(
            decide("A", false, None, Some(&trust("A", true))),
            Decision::AcceptAndRemember
        );
    }

    #[test]
    fn changed_certificates_are_refused_even_when_trusted() {
        assert_eq!(decide("A", false, Some("B"), None), Decision::Changed);
        assert_eq!(
            decide("A", false, Some("B"), Some(&trust("A", true))),
            Decision::Changed
        );
    }

    // Storage

    #[test]
    fn remembered_certificates_are_found_per_host_and_port() {
        let directory =
            std::env::temp_dir().join(format!("hoplix-certificates-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let store = directory.join("trusted_certificates");
        assert_eq!(stored_fingerprint(&store, "Example.test", 21), None);
        remember(&store, "Example.test", 21, "SHA256:AA").expect("writing failed");
        remember(&store, "::1", 990, "SHA256:BB").expect("writing failed");
        assert_eq!(
            stored_fingerprint(&store, "example.test", 21).as_deref(),
            Some("SHA256:AA")
        );
        assert_eq!(stored_fingerprint(&store, "example.test", 990), None);
        assert_eq!(
            stored_fingerprint(&store, "::1", 990).as_deref(),
            Some("SHA256:BB")
        );
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&store)
            .expect("opening failed");
        writeln!(file, "Edited.Example.test:21 SHA256:CC").expect("writing failed");
        drop(file);
        assert_eq!(
            stored_fingerprint(&store, "edited.example.TEST", 21).as_deref(),
            Some("SHA256:CC")
        );
        fs::remove_dir_all(directory).expect("cleaning up failed");
    }
}
