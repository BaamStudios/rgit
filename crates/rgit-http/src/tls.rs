// SPDX-License-Identifier: MIT
//! TLS client configuration on rustls (ring provider).
//!
//! - Trust roots: the system store (`rustls-native-certs`), or only the
//!   PEM bundle of `http.sslCAInfo` and the PEM files in `http.sslCAPath`
//!   when one of them is set (libcurl replaces its default bundle the same
//!   way).
//! - `http.sslVerify=false` / `GIT_SSL_NO_VERIFY`: any certificate is
//!   accepted; handshake signatures are still checked.
//! - ALPN offers `http/1.1` only.

use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::error::Transport;
use crate::messages;
use crate::options::HttpOptions;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Build the client configuration for `opts`.
pub(crate) fn client_config(opts: &HttpOptions) -> Result<Arc<ClientConfig>, Transport> {
    let builder = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| Transport::new(35, messages::ssl_connect_error(&e.to_string())))?;
    let mut cfg = if opts.ssl_verify {
        let roots = roots(opts)?;
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(provider())))
            .with_no_client_auth()
    };
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

fn roots(opts: &HttpOptions) -> Result<RootCertStore, Transport> {
    let mut store = RootCertStore::empty();
    if opts.ssl_ca_info.is_none() && opts.ssl_ca_path.is_none() {
        let found = rustls_native_certs::load_native_certs();
        for c in found.certs {
            let _ = store.add(c);
        }
        return Ok(store);
    }
    if let Some(file) = &opts.ssl_ca_info {
        add_pem_file(&mut store, file)
            .map_err(|_| Transport::new(77, messages::ca_file_problem(&file.to_string_lossy())))?;
    }
    if let Some(dir) = &opts.ssl_ca_path {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for ent in rd.flatten() {
                let _ = add_pem_file(&mut store, &ent.path());
            }
        }
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
pub(crate) fn server_name(host: &str) -> Result<ServerName<'static>, Transport> {
    ServerName::try_from(host.to_string())
        .map_err(|_| Transport::new(3, messages::invalid_url(host)))
}

/// Map a handshake error to libcurl's wording.
pub(crate) fn map_error(e: &std::io::Error, host: &str) -> Transport {
    let inner = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>());
    match inner {
        Some(rustls::Error::InvalidCertificate(ce)) => {
            use rustls::CertificateError as C;
            let reason = match ce {
                C::UnknownIssuer => messages::SSL_UNKNOWN_ISSUER.to_string(),
                C::Expired | C::ExpiredContext { .. } => messages::SSL_EXPIRED.to_string(),
                C::NotValidForName | C::NotValidForNameContext { .. } => {
                    messages::ssl_name_mismatch(host)
                }
                other => messages::ssl_problem(&format!("{other:?}")),
            };
            Transport::new(60, reason)
        }
        Some(other) => Transport::new(35, messages::ssl_connect_error(&other.to_string())),
        None => Transport::new(35, messages::ssl_connect_error(&e.to_string())),
    }
}

/// Accepts every certificate (`http.sslVerify=false`).
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
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
            &self.0.signature_verification_algorithms,
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
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
