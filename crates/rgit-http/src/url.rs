// SPDX-License-Identifier: MIT
//! `http://` and `https://` URLs as the smart HTTP client needs them.
//!
//! The parser is deliberately small: scheme, optional `user[:password]@`
//! (percent-decoded), host (an IPv6 literal in brackets is kept without
//! them), optional port, and the rest of the URL (path and query) verbatim.
//! Request URLs are built by appending to the repository URL as described
//! in `gitprotocol-http(5)`, "URL Format".

use std::fmt;

use crate::error::Error;

/// `http` or `https`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    /// Plain HTTP.
    Http,
    /// HTTP over TLS.
    Https,
}

impl Scheme {
    /// The scheme name.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }

    /// The default port.
    pub fn default_port(self) -> u16 {
        match self {
            Scheme::Http => 80,
            Scheme::Https => 443,
        }
    }
}

/// A parsed HTTP(S) URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Scheme.
    pub scheme: Scheme,
    /// Decoded user name, if the URL has a user part.
    pub user: Option<String>,
    /// Decoded password, if the URL has one.
    pub password: Option<String>,
    /// Host name or IP literal (IPv6 without brackets).
    pub host: String,
    /// Port as written.
    pub port: Option<u16>,
    /// Path and query, starting with `/`.
    pub path: String,
    /// The user information exactly as written (for faithful display).
    raw_userinfo: Option<String>,
}

impl Url {
    /// Parse an absolute `http://` or `https://` URL.
    pub fn parse(input: &str) -> Result<Url, Error> {
        let bad = || Error::InvalidUrl(input.to_string());
        let (scheme, rest) = input.split_once("://").ok_or_else(bad)?;
        let scheme = if scheme.eq_ignore_ascii_case("http") {
            Scheme::Http
        } else if scheme.eq_ignore_ascii_case("https") {
            Scheme::Https
        } else {
            return Err(Error::UnsupportedScheme(scheme.to_string()));
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        let tail = tail.split('#').next().unwrap_or("");
        let path = if tail.starts_with('/') {
            tail.to_string()
        } else {
            format!("/{tail}")
        };
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
            None => (None, authority),
        };
        let (user, password) = match userinfo {
            Some(ui) => match ui.split_once(':') {
                Some((u, p)) => (Some(percent_decode(u)), Some(percent_decode(p))),
                None => (Some(percent_decode(ui)), None),
            },
            None => (None, None),
        };
        let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
            let close = v6.find(']').ok_or_else(bad)?;
            let host = &v6[..close];
            let after = &v6[close + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) if !p.is_empty() => Some(p.parse().map_err(|_| bad())?),
                Some(_) => None,
                None if after.is_empty() => None,
                None => return Err(bad()),
            };
            (host.to_string(), port)
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() => {
                    (h.to_string(), Some(p.parse().map_err(|_| bad())?))
                }
                Some((h, _)) => (h.to_string(), None),
                None => (hostport.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err(bad());
        }
        Ok(Url {
            scheme,
            user,
            password,
            host,
            port,
            path,
            raw_userinfo: userinfo.map(str::to_string),
        })
    }

    /// The effective port.
    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or(self.scheme.default_port())
    }

    /// The host as written in a URL or `Host` header (IPv6 in brackets).
    pub fn host_for_url(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// `host[:port]` as in the `Host` header: the port only if the URL has
    /// one.
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host_for_url()),
            None => self.host_for_url(),
        }
    }

    /// `host:port` with the effective port (for `CONNECT` and sockets).
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host_for_url(), self.port_or_default())
    }

    /// The URL without user name and password.
    pub fn without_credentials(&self) -> Url {
        Url {
            user: None,
            password: None,
            raw_userinfo: None,
            ..self.clone()
        }
    }

    /// `scheme://host[:port]`, used as the credential prompt location.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme.as_str(), self.authority())
    }

    /// Whether two URLs point to the same scheme, host and port.
    pub fn same_origin(&self, other: &Url) -> bool {
        self.scheme == other.scheme
            && self.host.eq_ignore_ascii_case(&other.host)
            && self.port_or_default() == other.port_or_default()
    }

    /// The repository URL with a trailing `/` removed.
    fn base_string(&self) -> String {
        let mut s = self.to_string();
        while s.ends_with('/') && s.len() > self.prefix_len() {
            s.pop();
        }
        s
    }

    fn prefix_len(&self) -> usize {
        let mut u = self.clone();
        u.path = String::new();
        u.to_string().len()
    }

    /// Append `suffix` (`info/refs?service=...` or `git-upload-pack`) to
    /// the repository URL: a trailing `/` is stripped first, and if the URL
    /// already has a query, the `?` of the suffix becomes `&`.
    pub fn join(&self, suffix: &str) -> Url {
        let base = self.base_string();
        let suffix = if self.path.contains('?') {
            suffix.replacen('?', "&", 1)
        } else {
            suffix.to_string()
        };
        let joined = format!("{base}/{suffix}");
        let mut out = Url::parse(&joined).unwrap_or_else(|_| self.clone());
        // Keep the user part exactly as the caller gave it.
        out.user = self.user.clone();
        out.password = self.password.clone();
        out.raw_userinfo = self.raw_userinfo.clone();
        out
    }

    /// The inverse of [`Url::join`]: if this URL ends in `/<suffix>`
    /// (with the same `?`/`&` rule), the repository URL in front of it.
    pub fn strip_suffix(&self, suffix: &str) -> Option<Url> {
        let s = self.to_string();
        for cand in [suffix.to_string(), suffix.replacen('?', "&", 1)] {
            if let Some(base) = s.strip_suffix(&format!("/{cand}")) {
                return Url::parse(base).ok();
            }
        }
        None
    }

    /// Resolve a `Location` header value against this URL.
    pub fn resolve(&self, location: &str) -> Result<Url, Error> {
        let location = location.trim();
        if location.contains("://") {
            return Url::parse(location);
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", self.scheme.as_str()));
        }
        let mut out = self.without_credentials();
        if location.starts_with('/') {
            out.path = location.to_string();
        } else if let Some(q) = location.strip_prefix('?') {
            let p = self.path.split('?').next().unwrap_or("/");
            out.path = format!("{p}?{q}");
        } else {
            let p = self.path.split('?').next().unwrap_or("/");
            let dir = &p[..p.rfind('/').map(|i| i + 1).unwrap_or(0)];
            out.path = format!("{dir}{location}");
        }
        Ok(out)
    }

    /// The path without the leading `/` and without the query, as used in
    /// credential requests.
    pub fn credential_path(&self) -> String {
        let p = self.path.split('?').next().unwrap_or("");
        p.trim_start_matches('/').to_string()
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme.as_str())?;
        if let Some(ui) = &self.raw_userinfo {
            write!(f, "{ui}@")?;
        } else if let Some(u) = &self.user {
            write!(f, "{}", percent_encode(u))?;
            if let Some(p) = &self.password {
                write!(f, ":{}", percent_encode(p))?;
            }
            write!(f, "@")?;
        }
        write!(f, "{}{}", self.authority(), self.path)
    }
}

/// Decode `%XX` escapes; invalid escapes are kept as written.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape characters that cannot appear in the user information.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let u = Url::parse("https://u%40h:p%3Aw@example.com:8443/a/b.git/?x").unwrap();
        assert_eq!(u.scheme, Scheme::Https);
        assert_eq!(u.user.as_deref(), Some("u@h"));
        assert_eq!(u.password.as_deref(), Some("p:w"));
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, Some(8443));
        assert_eq!(u.path, "/a/b.git/?x");
        assert_eq!(
            u.to_string(),
            "https://u%40h:p%3Aw@example.com:8443/a/b.git/?x"
        );
        assert_eq!(
            u.without_credentials().to_string(),
            "https://example.com:8443/a/b.git/?x"
        );
    }

    #[test]
    fn parse_ipv6_and_errors() {
        let u = Url::parse("http://[::1]:99/r").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.authority(), "[::1]:99");
        assert!(Url::parse("ftp://x/y").is_err());
        assert!(Url::parse("http:///y").is_err());
        assert!(Url::parse("nonsense").is_err());
    }

    #[test]
    fn join_strips_slash_and_handles_query() {
        let u = Url::parse("http://example.com/git/repo.git/").unwrap();
        assert_eq!(
            u.join("info/refs?service=git-upload-pack").to_string(),
            "http://example.com/git/repo.git/info/refs?service=git-upload-pack"
        );
        let u = Url::parse("http://example.com/daemon.cgi?svc=git&q=").unwrap();
        assert_eq!(
            u.join("info/refs?service=git-receive-pack").to_string(),
            "http://example.com/daemon.cgi?svc=git&q=/info/refs&service=git-receive-pack"
        );
        let j = u.join("info/refs?service=git-receive-pack");
        assert_eq!(
            j.strip_suffix("info/refs?service=git-receive-pack")
                .unwrap()
                .to_string(),
            "http://example.com/daemon.cgi?svc=git&q="
        );
    }

    #[test]
    fn resolve_locations() {
        let u = Url::parse("http://a:1/x/y/info/refs?service=s").unwrap();
        assert_eq!(u.resolve("/z").unwrap().to_string(), "http://a:1/z");
        assert_eq!(u.resolve("w").unwrap().to_string(), "http://a:1/x/y/info/w");
        assert_eq!(u.resolve("//b/c").unwrap().to_string(), "http://b/c");
        assert_eq!(u.resolve("https://c/d").unwrap().to_string(), "https://c/d");
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode("a%2fb%zz%"), "a/b%zz%");
        assert_eq!(percent_encode("a@b c"), "a%40b%20c");
    }
}
