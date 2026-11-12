// SPDX-License-Identifier: MIT
//! The `<url>` of `credential.<url>.*` keys and how it matches a
//! credential (`Documentation/gitcredentials.adoc`, "CREDENTIAL CONTEXTS",
//! and observations of git 2.56):
//!
//! - The URL may be partial: `example.com`, `user@example.com/org`,
//!   `https://`, `/org/repo.git`. Missing parts match anything.
//! - Scheme and host compare case-insensitively; a default port (`:443`
//!   for https, `:80` for http) is the same as no port.
//! - A host label `*` (or `*` inside a label) is a wildcard within one
//!   label: `*.org` matches `example.org` but not `a.b.org`.
//! - A user name in the pattern must equal the credential's user name.
//! - The pattern path must be a prefix of the credential's path at a `/`
//!   boundary (`org` matches `org/repo.git`, `o` does not).
//! - Components are percent-decoded before comparing.

use bstr::{BString, ByteSlice};

use super::{Credential, check_newlines, split_authority};

/// A parsed `credential.<url>` subsection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    protocol: Option<BString>,
    username: Option<BString>,
    host: Option<BString>,
    path: Option<BString>,
}

impl Pattern {
    /// Parse a possibly partial URL. `None` (after a warning) if a decoded
    /// component contains a newline.
    pub fn parse(s: &[u8]) -> Option<Pattern> {
        let (protocol, rest) = match s.find(b"://") {
            Some(i) => (Some(BString::from(&s[..i])), &s[i + 3..]),
            None => (None, s),
        };
        let parts = split_authority(rest);
        if !check_newlines(&parts, s) {
            return None;
        }
        Some(Pattern {
            protocol: protocol.filter(|p| !p.is_empty()),
            username: parts.username,
            host: Some(parts.host).filter(|h| !h.is_empty()),
            path: parts.path,
        })
    }

    /// Whether the pattern applies to `cred`.
    pub fn matches(&self, cred: &Credential) -> bool {
        let proto = cred.protocol.as_deref().map(|p| p.as_bytes());
        if let Some(p) = &self.protocol {
            match proto {
                Some(cp) if cp.eq_ignore_ascii_case(p) => {}
                _ => return false,
            }
        }
        if let Some(h) = &self.host {
            match &cred.host {
                Some(ch) if host_matches(h, ch, proto.unwrap_or_default()) => {}
                _ => return false,
            }
        }
        if let Some(u) = &self.username {
            if cred.username.as_ref() != Some(u) {
                return false;
            }
        }
        if let Some(p) = &self.path {
            let cp: &[u8] = cred
                .path
                .as_deref()
                .map(|p| p.as_bytes())
                .unwrap_or_default();
            let cp = trim_slashes(cp);
            if !(cp == p.as_bytes() || (cp.starts_with(p) && cp[p.len()] == b'/')) {
                return false;
            }
        }
        true
    }
}

fn trim_slashes(mut p: &[u8]) -> &[u8] {
    while p.first() == Some(&b'/') {
        p = &p[1..];
    }
    while p.last() == Some(&b'/') {
        p = &p[..p.len() - 1];
    }
    p
}

/// Lower-case host and port with the scheme's default port removed.
fn normalize_host(h: &[u8], proto: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let lower = h.to_ascii_lowercase();
    let (host, port) = match lower.rfind_byte(b':') {
        Some(i) if !lower[i + 1..].is_empty() && lower[i + 1..].iter().all(u8::is_ascii_digit) => {
            (lower[..i].to_vec(), lower[i + 1..].to_vec())
        }
        _ => (lower.clone(), Vec::new()),
    };
    let default: &[u8] = match proto.to_ascii_lowercase().as_slice() {
        b"https" => b"443",
        b"http" => b"80",
        _ => b"",
    };
    let port = if port.as_slice() == default {
        Vec::new()
    } else {
        port
    };
    (host, port)
}

fn host_matches(pattern: &[u8], host: &[u8], proto: &[u8]) -> bool {
    let (ph, pp) = normalize_host(pattern, proto);
    let (ch, cp) = normalize_host(host, proto);
    if pp != cp {
        return false;
    }
    let pl: Vec<&[u8]> = ph.split(|&b| b == b'.').collect();
    let cl: Vec<&[u8]> = ch.split(|&b| b == b'.').collect();
    pl.len() == cl.len() && pl.iter().zip(&cl).all(|(p, c)| glob(p, c))
}

/// `*` matches any run of characters (within one label).
fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => (0..=s.len()).any(|i| glob(&p[1..], &s[i..])),
        Some(&c) => s.first() == Some(&c) && glob(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(url: &str) -> Credential {
        let mut c = Credential::new();
        c.set_url(url.as_bytes()).unwrap();
        c
    }

    fn m(pattern: &str, url: &str) -> bool {
        Pattern::parse(pattern.as_bytes())
            .unwrap()
            .matches(&cred(url))
    }

    #[test]
    fn partial_urls() {
        let url = "https://user@example.com/org/repo.git";
        for p in [
            "example.com",
            "example.com/org/repo.git",
            "user@example.com",
            "user@example.com/org/repo.git",
            "https://",
            "https://example.com",
            "https://example.com/",
            "https://example.com/org",
            "https://example.com/org/",
            "https://example.com/org/repo.git",
            "https://user@example.com",
            "https://user@example.com/",
            "https://user@example.com/org",
            "https://user@example.com/org/",
            "https://user@example.com/org/repo.git",
            "/org/repo.git",
        ] {
            assert!(m(p, url), "{p}");
        }
        for p in [
            "dont.use.this",
            "example.com/o",
            "user@example.com/o",
            "http://",
            "https://example.com/o",
            "https://user@example.com/o",
            "/o",
            "/repo",
        ] {
            assert!(!m(p, url), "{p}");
        }
    }

    #[test]
    fn hosts() {
        assert!(m("https://*.org", "https://example.org/x"));
        assert!(!m("https://*.example.com", "https://example.com/"));
        assert!(m("https://*.com", "https://example.com/"));
        assert!(m("HTTPS://EXAMPLE.COM", "https://example.com/"));
        assert!(m("https://example.com:443", "https://example.com/"));
        assert!(m("https://example.com", "https://example.com:443/"));
        assert!(!m("https://example.com", "https://example.com:8080/"));
        assert!(m("https://a%20b%20c/", "https://a b c/"));
        assert!(m(
            "https://user%2fname@example.com/foo/bar.git",
            "https://user%2fname@example.com/foo/bar.git"
        ));
        assert!(m(
            "https://example.com/%2566.git",
            "https://example.com/%2566.git"
        ));
        assert!(!m("https://alice@example.com", "https://bob@example.com"));
    }

    #[test]
    fn newline_in_pattern() {
        assert!(Pattern::parse(b"with%0anewline").is_none());
    }
}
