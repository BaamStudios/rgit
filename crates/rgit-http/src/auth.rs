// SPDX-License-Identifier: MIT
//! HTTP authentication: the hook into the credential subsystem and Basic
//! auth encoding.
//!
//! The flow follows `gitcredentials(7)`: a request that gets `401` asks the
//! [`CredentialProvider`] to fill a [`Credential`] (protocol, host, path and
//! any user name from the URL), retries with Basic auth, and reports the
//! outcome with `approve` (success) or `reject` (another `401`).
//!
//! The provider is a trait so the credential helpers (`rgit-transport`,
//! issue #161) can be plugged in without this crate depending on them.

/// One credential, in the terms of the credential helper protocol.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Credential {
    /// `http` or `https`.
    pub protocol: String,
    /// `host` or `host:port`.
    pub host: String,
    /// Repository path without the leading `/` (helpers use it only with
    /// `credential.useHttpPath`).
    pub path: Option<String>,
    /// User name.
    pub username: Option<String>,
    /// Password or token.
    pub password: Option<String>,
}

impl Credential {
    /// The `Authorization` value for Basic auth.
    pub fn basic_authorization(&self) -> String {
        let user = self.username.as_deref().unwrap_or("");
        let pass = self.password.as_deref().unwrap_or("");
        format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()))
    }
}

/// The credential subsystem as seen by the HTTP client.
pub trait CredentialProvider {
    /// Complete `cred` (at least `username` and `password`). An error
    /// message is printed as `fatal: <message>` by the caller, for example
    /// `could not read Username for 'https://host': terminal prompts disabled`.
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String>;

    /// The credential worked.
    fn approve(&mut self, cred: &Credential);

    /// The credential was refused.
    fn reject(&mut self, cred: &Credential);
}

impl<T: CredentialProvider + ?Sized> CredentialProvider for &mut T {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        (**self).fill(cred)
    }
    fn approve(&mut self, cred: &Credential) {
        (**self).approve(cred)
    }
    fn reject(&mut self, cred: &Credential) {
        (**self).reject(cred)
    }
}

impl<T: CredentialProvider + ?Sized> CredentialProvider for Box<T> {
    fn fill(&mut self, cred: &mut Credential) -> Result<(), String> {
        (**self).fill(cred)
    }
    fn approve(&mut self, cred: &Credential) {
        (**self).approve(cred)
    }
    fn reject(&mut self, cred: &Credential) {
        (**self).reject(cred)
    }
}

/// Standard base64 with padding (RFC 4648), as used by Basic auth.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors() {
        // RFC 4648, section 10.
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn basic() {
        let c = Credential {
            username: Some("user".into()),
            password: Some("pass".into()),
            ..Default::default()
        };
        assert_eq!(c.basic_authorization(), "Basic dXNlcjpwYXNz");
    }
}
