//! rustls client configs for `postgres-tls` / `elasticsearch-tls`.
//!
//! All configs use the `ring` provider explicitly (no aws-lc-rs, no process-wide default).
//!
//! | function                      | verifies                         | use for |
//! |-------------------------------|----------------------------------|---------|
//! | [`native_roots_config`]       | chain (OS store) + hostname      | public CA |
//! | [`config_with_ca_pem`]        | chain (your CA) + hostname       | self-signed / private CA |
//! | [`ca_only_config`]            | chain only, any hostname         | Postgres `VerifyCA` |
//! | [`pinned_config`]             | SHA-256 of a cert in the chain   | Elasticsearch fingerprint |
//! | [`insecure_config`]           | nothing (encryption only)        | `TrustServerCertificate=true` |

use std::sync::Arc;

pub use rustls::pki_types;
pub use rustls::{ClientConfig, RootCertStore};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, Error, SignatureScheme};

use crate::BoxError;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn builder() -> Result<rustls::ConfigBuilder<ClientConfig, rustls::WantsVerifier>, BoxError> {
    Ok(ClientConfig::builder_with_provider(provider()).with_safe_default_protocol_versions()?)
}

/// Roots from the OS certificate store (macOS keychain, Linux /etc/ssl, Windows store).
pub fn native_roots() -> Result<RootCertStore, BoxError> {
    let mut roots = RootCertStore::empty();
    let loaded = rustls_native_certs::load_native_certs();
    for cert in loaded.certs {
        let _ = roots.add(cert); // skip the odd unparsable cert
    }
    if roots.is_empty() {
        let detail = loaded
            .errors
            .first()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "store is empty".into());
        return Err(format!("no usable root certificates in the OS store: {detail}").into());
    }
    Ok(roots)
}

/// Roots from PEM data (one or more `-----BEGIN CERTIFICATE-----` blocks).
pub fn roots_from_pem(pem: &[u8]) -> Result<RootCertStore, BoxError> {
    let mut roots = RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem) {
        roots.add(cert?)?;
    }
    if roots.is_empty() {
        return Err("no certificate found in PEM data".into());
    }
    Ok(roots)
}

/// Full verification against the OS trust store.
pub fn native_roots_config() -> Result<ClientConfig, BoxError> {
    config_with_roots(native_roots()?)
}

/// Full verification (chain + hostname) against `roots` only.
pub fn config_with_roots(roots: RootCertStore) -> Result<ClientConfig, BoxError> {
    Ok(builder()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// Full verification against a private / self-signed CA given as PEM.
/// The server certificate must still name the host you connect to.
pub fn config_with_ca_pem(pem: &[u8]) -> Result<ClientConfig, BoxError> {
    config_with_roots(roots_from_pem(pem)?)
}

/// Chain must lead to `roots`, hostname is not checked (libpq/Npgsql `verify-ca`).
pub fn ca_only_config(roots: RootCertStore) -> Result<ClientConfig, BoxError> {
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider()).build()?;
    Ok(builder()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(CaOnly(inner)))
        .with_no_client_auth())
}

/// Accept the server if any certificate it presents has this SHA-256 fingerprint
/// (hex, `:` separators allowed). This is what Elasticsearch prints at setup
/// (`ca_trusted_fingerprint` / "HTTP CA certificate SHA-256 fingerprint").
pub fn pinned_config<I, S>(fingerprints: I) -> Result<ClientConfig, BoxError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let pins = fingerprints
        .into_iter()
        .map(|f| parse_sha256_hex(f.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    if pins.is_empty() {
        return Err("at least one fingerprint is required".into());
    }
    Ok(builder()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned {
            pins,
            provider: provider(),
        }))
        .with_no_client_auth())
}

/// Encrypt but accept any certificate. Equivalent of `TrustServerCertificate=true`
/// and of libpq/Npgsql `sslmode=require`. Protects against passive sniffing, not MITM.
pub fn insecure_config() -> Result<ClientConfig, BoxError> {
    Ok(builder()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider())))
        .with_no_client_auth())
}

fn parse_sha256_hex(s: &str) -> Result<[u8; 32], BoxError> {
    let hex: String = s
        .chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect();
    if hex.len() != 64 {
        return Err(format!(
            "SHA-256 fingerprint must be 64 hex digits, got {}",
            hex.len()
        )
        .into());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
    }
    Ok(out)
}

fn signature_tls12(
    p: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    verify_tls12_signature(message, cert, dss, &p.signature_verification_algorithms)
}

fn signature_tls13(
    p: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    verify_tls13_signature(message, cert, dss, &p.signature_verification_algorithms)
}

#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        signature_tls12(&self.0, m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        signature_tls13(&self.0, m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug)]
struct Pinned {
    pins: Vec<[u8; 32]>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let matched = std::iter::once(end_entity).chain(intermediates).any(|c| {
            let digest = ring::digest::digest(&ring::digest::SHA256, c.as_ref());
            self.pins.iter().any(|p| p[..] == *digest.as_ref())
        });
        if matched {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General(
                "no presented certificate matches the pinned SHA-256 fingerprint".into(),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        signature_tls12(&self.provider, m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        signature_tls13(&self.provider, m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct CaOnly(Arc<WebPkiServerVerifier>);

impl ServerCertVerifier for CaOnly {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
        {
            Err(Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls12_signature(m, c, d)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.0.verify_tls13_signature(m, c, d)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_parsing() {
        let hex = "AB:".repeat(31) + "AB";
        assert_eq!(parse_sha256_hex(&hex).unwrap(), [0xab; 32]);
        assert_eq!(parse_sha256_hex(&"0f".repeat(32)).unwrap(), [0x0f; 32]);
        assert!(parse_sha256_hex("abcd").is_err());
        assert!(parse_sha256_hex(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn configs_build() {
        insecure_config().unwrap();
        pinned_config(["00".repeat(32)]).unwrap();
        assert!(roots_from_pem(b"not a pem").is_err());
    }
}
