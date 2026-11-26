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
//! - `http.sslCert` / `http.sslKey`: a PEM client certificate chain and
//!   its PEM private key (PKCS#8, PKCS#1 or SEC1; the key may be in the
//!   certificate file). Other types and encrypted keys are not supported.
//! - `http.sslVersion`: `tlsv1.3` restricts to TLS 1.3; the older TLS
//!   versions mean "TLS 1.2 or later" (rustls has nothing older);
//!   `sslv2`/`sslv3` fail.
//! - `http.sslCipherList`: OpenSSL or IANA names of the suites rustls
//!   implements, separated by `:`, `,` or spaces; a list naming no TLS 1.3
//!   suite leaves TLS 1.3 alone (as OpenSSL does); a list that selects
//!   nothing fails like libcurl (`failed setting cipher list: ...`).
//! - `http.pinnedPubkey`: `sha256//<base64>` hashes (several separated by
//!   `;`) or a PEM/DER public key file; checked against the server's key
//!   after the normal verification.

use std::path::Path;
use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, ServerName, SubjectPublicKeyInfoDer, UnixTime,
};
use rustls::{
    ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme, SupportedCipherSuite,
};
use sha2::{Digest, Sha256};

use crate::error::Transport;
use crate::messages;
use crate::options::{HttpOptions, SslVersion};

/// OpenSSL's names for the suites rustls implements.
const OPENSSL_NAMES: &[(&str, &str)] = &[
    ("TLS_AES_128_GCM_SHA256", "TLS13_AES_128_GCM_SHA256"),
    ("TLS_AES_256_GCM_SHA384", "TLS13_AES_256_GCM_SHA384"),
    (
        "TLS_CHACHA20_POLY1305_SHA256",
        "TLS13_CHACHA20_POLY1305_SHA256",
    ),
    (
        "ECDHE-ECDSA-AES128-GCM-SHA256",
        "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
    ),
    (
        "ECDHE-ECDSA-AES256-GCM-SHA384",
        "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
    ),
    (
        "ECDHE-ECDSA-CHACHA20-POLY1305",
        "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256",
    ),
    (
        "ECDHE-RSA-AES128-GCM-SHA256",
        "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
    ),
    (
        "ECDHE-RSA-AES256-GCM-SHA384",
        "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384",
    ),
    (
        "ECDHE-RSA-CHACHA20-POLY1305",
        "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256",
    ),
];

fn suite_name(s: &SupportedCipherSuite) -> String {
    format!("{:?}", s.suite())
}

/// Apply `http.sslCipherList` to the provider's suites.
fn select_suites(
    list: &str,
    all: &[SupportedCipherSuite],
) -> Result<Vec<SupportedCipherSuite>, Transport> {
    let wanted: Vec<String> = list
        .split([':', ',', ' '])
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| {
            OPENSSL_NAMES
                .iter()
                .find(|(o, _)| o.eq_ignore_ascii_case(n))
                .map_or_else(|| n.to_ascii_uppercase(), |(_, i)| i.to_string())
        })
        .collect();
    let is13 = |s: &SupportedCipherSuite| matches!(s, SupportedCipherSuite::Tls13(_));
    let named = |s: &SupportedCipherSuite| wanted.contains(&suite_name(s));
    let names_13 = all.iter().any(|s| is13(s) && named(s));
    let picked: Vec<SupportedCipherSuite> = all
        .iter()
        .filter(|s| named(s) || (is13(s) && !names_13))
        .copied()
        .collect();
    if !all.iter().any(named) {
        return Err(Transport::new(59, messages::cipher_list_failed(list)));
    }
    Ok(picked)
}

/// Build the client configuration for `opts`.
pub(crate) fn client_config(opts: &HttpOptions) -> Result<Arc<ClientConfig>, Transport> {
    let mut prov = rustls::crypto::ring::default_provider();
    if let Some(list) = &opts.ssl_cipher_list {
        prov.cipher_suites = select_suites(list, &prov.cipher_suites)?;
    }
    let prov = Arc::new(prov);
    let versions: &[&'static rustls::SupportedProtocolVersion] = match opts.ssl_version {
        SslVersion::Tls13 => &[&rustls::version::TLS13],
        SslVersion::Ssl => {
            return Err(Transport::new(
                35,
                messages::ssl_version_unsupported("SSLv2/SSLv3"),
            ));
        }
        SslVersion::Default | SslVersion::Tls12 => rustls::DEFAULT_VERSIONS,
    };
    let builder = ClientConfig::builder_with_provider(prov.clone())
        .with_protocol_versions(versions)
        .map_err(|e| Transport::new(35, messages::ssl_connect_error(&e.to_string())))?;
    let pins = match &opts.pinned_pubkey {
        Some(p) => Some(parse_pins(p)?),
        None => None,
    };
    let verifier: Option<Arc<dyn ServerCertVerifier>> = match (opts.ssl_verify, pins) {
        (true, None) => None,
        (true, Some(pins)) => {
            let inner =
                WebPkiServerVerifier::builder_with_provider(Arc::new(roots(opts)?), prov.clone())
                    .build()
                    .map_err(|e| Transport::new(77, messages::ssl_problem(&e.to_string())))?;
            Some(Arc::new(Pinned { inner, pins }))
        }
        (false, None) => Some(Arc::new(NoVerify(prov.clone()))),
        (false, Some(pins)) => Some(Arc::new(Pinned {
            inner: Arc::new(NoVerify(prov.clone())),
            pins,
        })),
    };
    let builder = match verifier {
        None => builder.with_root_certificates(roots(opts)?),
        Some(v) => builder.dangerous().with_custom_certificate_verifier(v),
    };
    let mut cfg = match &opts.ssl_cert {
        Some(cert) => {
            for kind in [&opts.ssl_cert_type, &opts.ssl_key_type]
                .into_iter()
                .flatten()
            {
                if !kind.eq_ignore_ascii_case("PEM") {
                    return Err(Transport::new(58, messages::unsupported_cert_type(kind)));
                }
            }
            let shown = cert.to_string_lossy();
            let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
                .map_err(|_| Transport::new(58, messages::client_cert_problem(&shown)))?
                .collect::<Result<_, _>>()
                .map_err(|_| Transport::new(58, messages::client_cert_problem(&shown)))?;
            if chain.is_empty() {
                return Err(Transport::new(58, messages::client_cert_problem(&shown)));
            }
            let key_file = opts.ssl_key.as_deref().unwrap_or(cert);
            let key = PrivateKeyDer::from_pem_file(key_file).map_err(|_| {
                Transport::new(
                    58,
                    messages::client_key_problem(&key_file.to_string_lossy()),
                )
            })?;
            builder.with_client_auth_cert(chain, key).map_err(|_| {
                Transport::new(
                    58,
                    messages::client_key_problem(&key_file.to_string_lossy()),
                )
            })?
        }
        None => builder.with_no_client_auth(),
    };
    cfg.alpn_protocols = if opts.version == crate::options::HttpVersion::Http11 {
        vec![b"http/1.1".to_vec()]
    } else {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    };
    Ok(Arc::new(cfg))
}

/// `http.pinnedPubkey` as SHA-256 hashes of SubjectPublicKeyInfo.
fn parse_pins(spec: &str) -> Result<Vec<[u8; 32]>, Transport> {
    if spec.starts_with("sha256//") {
        let mut pins = Vec::new();
        for part in spec.split(';') {
            let b64 = part.trim().strip_prefix("sha256//").unwrap_or("");
            if let Some(d) = base64_decode(b64).and_then(|d| <[u8; 32]>::try_from(d).ok()) {
                pins.push(d);
            }
        }
        return Ok(pins);
    }
    let unreadable = || Transport::new(90, messages::PINNED_KEY_MISMATCH);
    let data = std::fs::read(spec).map_err(|_| unreadable())?;
    let der = match SubjectPublicKeyInfoDer::from_pem_slice(&data) {
        Ok(k) => k.as_ref().to_vec(),
        Err(_) => data,
    };
    Ok(vec![Sha256::digest(&der).into()])
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for &c in s {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// One DER element: (tag, header length, content length).
fn der_element(d: &[u8]) -> Option<(u8, usize, usize)> {
    let tag = *d.first()?;
    let first = *d.get(1)? as usize;
    if first < 0x80 {
        return Some((tag, 2, first));
    }
    let n = first & 0x7f;
    if n == 0 || n > 4 {
        return None;
    }
    let mut len = 0usize;
    for i in 0..n {
        len = (len << 8) | *d.get(2 + i)? as usize;
    }
    Some((tag, 2 + n, len))
}

/// The SubjectPublicKeyInfo of an X.509 certificate (RFC 5280, 4.1).
fn certificate_spki(cert: &[u8]) -> Option<&[u8]> {
    let (_, h, _) = der_element(cert)?; // Certificate
    let tbs = &cert[h..];
    let (_, h, len) = der_element(tbs)?; // TBSCertificate
    let mut body = tbs.get(h..h + len)?;
    let skip = |b: &mut &[u8]| -> Option<()> {
        let (_, h, l) = der_element(b)?;
        *b = b.get(h + l..)?;
        Some(())
    };
    if body.first() == Some(&0xa0) {
        skip(&mut body)?; // [0] version
    }
    for _ in 0..5 {
        skip(&mut body)?; // serial, signature, issuer, validity, subject
    }
    let (_, h, l) = der_element(body)?;
    body.get(..h + l)
}

/// The pin check on top of another verifier.
#[derive(Debug)]
struct Pinned {
    inner: Arc<dyn ServerCertVerifier>,
    pins: Vec<[u8; 32]>,
}

/// The marker of a pin mismatch inside `rustls::Error::General`.
pub(crate) const PIN_MISMATCH: &str = "rgit: pinned public key mismatch";

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let ok = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;
        let spki = certificate_spki(end_entity.as_ref())
            .ok_or_else(|| rustls::Error::General(PIN_MISMATCH.into()))?;
        let hash: [u8; 32] = Sha256::digest(spki).into();
        if self.pins.contains(&hash) {
            Ok(ok)
        } else {
            Err(rustls::Error::General(PIN_MISMATCH.into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
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
        Some(rustls::Error::General(m)) if m == PIN_MISMATCH => {
            Transport::new(90, messages::PINNED_KEY_MISMATCH)
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
