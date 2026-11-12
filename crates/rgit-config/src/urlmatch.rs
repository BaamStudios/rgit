// SPDX-License-Identifier: MIT
//! URL-specific configuration (`<section>.<url>.<key>`), as used by
//! `http.<url>.*`, `credential.<url>.*` and `git config get --url`.
//!
//! Written from git's documentation (`http.<url>.*` in git-config(1)): a
//! configuration URL matches a URL when scheme, host (with `*` matching one
//! whole host label or part of it), port (defaults filled in) and user name
//! (if the configuration names one) agree and its path is the URL's path or
//! a prefix of it ending at a `/` boundary. Better matches win in this
//! order: exact host over wildcard host, longer path, matching user name.
//! Among equally good matches the later one wins, and a plain
//! `<section>.<key>` is the fallback.

use bstr::{BStr, BString, ByteSlice};

use crate::key::split_canonical;
use crate::set::Entry;

/// A normalized URL, split into the parts used for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// Lower-cased scheme.
    pub scheme: String,
    /// User name, if any (the password is dropped).
    pub user: Option<String>,
    /// Lower-cased host; may contain `*` in a configuration URL.
    pub host: String,
    /// Port, with the scheme's default filled in when known.
    pub port: Option<u16>,
    /// Normalized path, always starting with `/`.
    pub path: String,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "ftp" => Some(21),
        "ftps" => Some(990),
        "git" => Some(9418),
        "ssh" => Some(22),
        _ => None,
    }
}

/// Normalize percent escapes: decode unreserved characters, upper-case the
/// hex digits of the rest.
fn normalize_escapes(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            let v = u8::from_str_radix(hex, 16).ok()?;
            if v.is_ascii_alphanumeric() || b"-._~".contains(&v) {
                out.push(v as char);
            } else {
                out.push('%');
                out.push_str(&hex.to_ascii_uppercase());
            }
            i += 3;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    Some(out)
}

/// Resolve `.` and `..` segments.
fn normalize_path(path: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    let trailing = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    for seg in path.split('/').skip(1) {
        match seg {
            "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segs.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

impl Url {
    /// Parse and normalize `scheme://[user[:pass]@]host[:port][/path]`.
    /// Query and fragment are ignored. Returns `None` for anything else.
    pub fn parse(url: &str) -> Option<Url> {
        let (scheme, rest) = url.split_once("://")?;
        let mut chars = scheme.chars();
        if !chars.next()?.is_ascii_alphabetic()
            || !chars.all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (user, hostport) = match authority.rfind('@') {
            Some(i) => {
                let userinfo = &authority[..i];
                let user = userinfo.split(':').next().unwrap_or("");
                (Some(normalize_escapes(user)?), &authority[i + 1..])
            }
            None => (None, authority),
        };
        let (host, port) = match hostport.rfind(':') {
            Some(i) if !hostport[i + 1..].contains(']') => {
                let p = &hostport[i + 1..];
                let port = if p.is_empty() {
                    None
                } else {
                    Some(p.parse::<u16>().ok()?)
                };
                (&hostport[..i], port)
            }
            _ => (hostport, None),
        };
        let host = normalize_escapes(host)?.to_ascii_lowercase();
        let port = port.or_else(|| default_port(&scheme));
        let path = normalize_path(&normalize_escapes(path)?);
        Some(Url {
            scheme,
            user,
            host,
            port,
            path,
        })
    }
}

/// How well a configuration URL matches; larger is better.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score {
    exact_host: bool,
    host_chars: usize,
    path_len: usize,
    user: bool,
}

/// `*` within one host label: glob without crossing a `.`.
fn label_matches(pattern: &str, label: &str) -> bool {
    match pattern.find('*') {
        None => pattern == label,
        Some(i) => {
            let (pre, post) = (&pattern[..i], &pattern[i + 1..]);
            if !label.starts_with(pre) {
                return false;
            }
            let rest = &label[pre.len()..];
            (0..=rest.len())
                .filter(|&k| rest.is_char_boundary(k))
                .any(|k| label_matches(post, &rest[k..]))
        }
    }
}

/// Whether configuration URL `config` matches `url`, and how well.
pub fn url_matches(config: &Url, url: &Url) -> Option<Score> {
    if config.scheme != url.scheme || config.port != url.port {
        return None;
    }
    let user = match &config.user {
        Some(u) => {
            if url.user.as_ref() != Some(u) {
                return None;
            }
            true
        }
        None => false,
    };
    let cl: Vec<&str> = config.host.split('.').collect();
    let ul: Vec<&str> = url.host.split('.').collect();
    if cl.len() != ul.len() || !cl.iter().zip(&ul).all(|(c, u)| label_matches(c, u)) {
        return None;
    }
    let cp = &config.path;
    let up = &url.path;
    let path_ok = cp == up
        || (up.starts_with(cp.as_str())
            && (cp.ends_with('/') || up.as_bytes().get(cp.len()) == Some(&b'/')));
    if !path_ok {
        return None;
    }
    Some(Score {
        exact_host: !config.host.contains('*'),
        host_chars: config.host.chars().filter(|c| *c != '*').count(),
        path_len: cp.trim_end_matches('/').len(),
        user,
    })
}

/// The entries of `section` that apply to `url`, one per variable name
/// (the best match each), sorted by variable name. Returns
/// `(canonical name without the URL, entry)`.
pub fn best_entries<'e>(
    entries: &'e [Entry],
    section: &BStr,
    key: Option<&BStr>,
    url: &Url,
) -> Vec<(BString, &'e Entry)> {
    let mut best: Vec<(BString, Option<Score>, &Entry)> = Vec::new();
    for e in entries {
        let (s, sub, name) = split_canonical(e.name.as_bstr());
        if s != section || key.is_some_and(|k| k != name) {
            continue;
        }
        let score = match sub {
            None => None,
            Some(sub) => {
                let Some(cu) = sub.to_str().ok().and_then(Url::parse) else {
                    continue;
                };
                match url_matches(&cu, url) {
                    Some(sc) => Some(sc),
                    None => continue,
                }
            }
        };
        let mut full = BString::from(s.as_bytes());
        full.push(b'.');
        full.extend_from_slice(name);
        match best.iter_mut().find(|(n, _, _)| *n == full) {
            Some(slot) => {
                if score >= slot.1 {
                    slot.1 = score;
                    slot.2 = e;
                }
            }
            None => best.push((full, score, e)),
        }
    }
    best.sort_by(|a, b| a.0.cmp(&b.0));
    best.into_iter().map(|(n, _, e)| (n, e)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(c: &str, u: &str) -> Option<Score> {
        url_matches(&Url::parse(c).unwrap(), &Url::parse(u).unwrap())
    }

    #[test]
    fn parse_normalizes() {
        let u = Url::parse("HTTPS://User:pw@Example.COM:443/a/./b/../c%7e%2f").unwrap();
        assert_eq!(u.scheme, "https");
        assert_eq!(u.user.as_deref(), Some("User"));
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, Some(443));
        assert_eq!(u.path, "/a/c~%2F");
        assert_eq!(Url::parse("https://x").unwrap().path, "/");
        assert!(Url::parse("x").is_none());
    }

    #[test]
    fn matching() {
        assert!(m("https://example.com", "https://example.com/x").is_some());
        assert!(m("https://example.com/sub", "https://example.com/sub/n").is_some());
        assert!(m("https://example.com/sub", "https://example.com/subx").is_none());
        assert!(m("https://*.example.com", "https://a.example.com").is_some());
        assert!(m("https://*.example.com", "https://a.b.example.com").is_none());
        assert!(m("https://*.example.com", "https://example.com").is_none());
        assert!(m("https://u@example.com", "https://example.com").is_none());
        assert!(m("https://example.com", "http://example.com").is_none());
        assert!(
            m("https://sub.example.com/", "https://sub.example.com/w")
                > m("https://*.example.com/w", "https://sub.example.com/w")
        );
        assert!(
            m("https://*.example.com", "https://a.example.com")
                > m("https://*.example.*", "https://a.example.com")
        );
    }
}
