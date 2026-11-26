// SPDX-License-Identifier: MIT
//! TLS client configuration on rustls (ring provider) for SMTP and IMAP.
//!
//! - Trust roots: the system store (`rustls-native-certs`), or only the
//!   PEM certificates of a CA file or directory when one is given.
//! - Verification can be switched off (`sendemail.smtpSSLCertPath=""`,
//!   `imap.sslverify=false`); handshake signatures are still checked.
//! - An optional PEM client certificate chain and private key.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

/// How to set up TLS.
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    /// Check the server certificate (and its name).
    pub verify: bool,
    /// Trust only the certificates of this PEM file or directory.
    pub ca_path: Option<PathBuf>,
    /// PEM client certificate chain.
    pub client_cert: Option<PathBuf>,
    /// PEM private key for the client certificate (default: in the
    /// certificate file).
    pub client_key: Option<PathBuf>,
}

impl TlsOptions {
    /// Verify against the system's trust store.
    pub fn verifying() -> TlsOptions {
        TlsOptions {
            verify: true,
            ..TlsOptions::default()
        }
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn io_err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// The client configuration for `opts`.
pub fn client_config(opts: &TlsOptions) -> std::io::Result<Arc<ClientConfig>> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| io_err(e.to_string()))?;
    let builder = if opts.verify {
        builder.with_root_certificates(roots(opts.ca_path.as_deref())?)
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny { provider }))
    };
    let config = match &opts.client_cert {
        Some(cert) => {
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
                .map_err(|e| io_err(format!("{}: {e}", cert.display())))?
                .collect::<Result<_, _>>()
                .map_err(|e| io_err(format!("{}: {e}", cert.display())))?;
            let key_file = opts.client_key.as_deref().unwrap_or(cert);
            let key = PrivateKeyDer::from_pem_file(key_file)
                .map_err(|e| io_err(format!("{}: {e}", key_file.display())))?;
            builder
                .with_client_auth_cert(chain, key)
                .map_err(|e| io_err(e.to_string()))?
        }
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

fn roots(ca_path: Option<&Path>) -> std::io::Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    match ca_path {
        None => {
            for c in rustls_native_certs::load_native_certs().certs {
                let _ = store.add(c);
            }
        }
        Some(p) if p.is_dir() => {
            for ent in std::fs::read_dir(p)?.flatten() {
                let _ = add_pem_file(&mut store, &ent.path());
            }
        }
        Some(p) => add_pem_file(&mut store, p)?,
    }
    Ok(store)
}

fn add_pem_file(store: &mut RootCertStore, path: &Path) -> std::io::Result<()> {
    let data = std::fs::read(path)?;
    for cert in CertificateDer::pem_slice_iter(&data).flatten() {
        let _ = store.add(cert);
    }
    Ok(())
}

/// The server name for SNI and verification.
pub fn server_name(host: &str) -> std::io::Result<ServerName<'static>> {
    ServerName::try_from(host.to_string()).map_err(|_| io_err(format!("invalid host name: {host}")))
}

/// Accepts every certificate; signatures are still verified.
#[derive(Debug)]
struct AcceptAny {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
