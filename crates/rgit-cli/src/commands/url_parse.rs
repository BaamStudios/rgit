// SPDX-License-Identifier: MIT
//! `git url-parse` (Documentation/git-url-parse.adoc): split git URLs
//! (`scheme://[user[:password]@]host[:port]/path` and the scp-like
//! `[user@]host:path`) into their components, or just validate them.
//!
//! Behaviour determined black-box against git 2.56.1: URLs are normalized
//! (lower-case scheme and host, percent escapes of unreserved characters
//! decoded and of others upper-cased, unsafe characters escaped, `.` and
//! `..` segments resolved); in the URL form the normalized URL is decoded
//! once more and normalized again before it is split. Query and fragment
//! are not part of any component. For `ssh`- and `git`-like schemes a
//! path starting with `/~` is printed without its leading slash.

use std::io::Write;

use crate::messages::url_parse as msg;
use crate::prelude::*;

const COMPONENTS: &[&str] = &["scheme", "user", "password", "host", "port", "path"];

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let component: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![Opt::string(
            'c',
            "component",
            &component,
            "component",
            msg::HELP_COMPONENT,
        )],
    );
    let urls = parser.parse(argv)?.args;
    if urls.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let component = component.into_inner();
    let index = match component.as_deref() {
        Some(c) => match COMPONENTS.iter().position(|&n| n == c) {
            Some(i) => Some(i),
            None => return Err(Die::Fatal(msg::invalid_component(c))),
        },
        None => None,
    };
    let mut out = std::io::stdout().lock();
    for url in &urls {
        let parts = match parse(url.as_bytes()) {
            Ok(p) => p,
            Err(e) => {
                let _ = out.flush();
                return Err(Die::Fatal(match e {
                    Error::NotUrl if url.starts_with('/') => msg::not_a_url_absolute(url),
                    Error::NotUrl => msg::not_a_url(url),
                    Error::Invalid(why) => msg::invalid_url(url, why),
                }));
            }
        };
        if let Some(i) = index {
            let value: &[u8] = match i {
                0 => &parts.scheme,
                1 => parts.user.as_deref().unwrap_or_default(),
                2 => parts.password.as_deref().unwrap_or_default(),
                3 => &parts.host,
                4 => parts.port.as_deref().unwrap_or_default(),
                _ => &parts.path,
            };
            let _ = out.write_all(value);
            let _ = out.write_all(b"\n");
        }
    }
    let _ = out.flush();
    Ok(0)
}

#[derive(Debug, PartialEq, Eq)]
enum Error {
    NotUrl,
    Invalid(&'static str),
}

/// The components of a parsed URL.
#[derive(Debug, Default, PartialEq, Eq)]
struct Parts {
    scheme: Vec<u8>,
    user: Option<Vec<u8>>,
    password: Option<Vec<u8>>,
    host: Vec<u8>,
    port: Option<Vec<u8>>,
    path: Vec<u8>,
    query: Option<Vec<u8>>,
    fragment: Option<Vec<u8>>,
}

impl Parts {
    fn serialize(&self) -> Vec<u8> {
        let mut s = self.scheme.clone();
        s.extend_from_slice(b"://");
        if self.user.is_some() || self.password.is_some() {
            s.extend_from_slice(self.user.as_deref().unwrap_or_default());
            if let Some(p) = &self.password {
                s.push(b':');
                s.extend_from_slice(p);
            }
            s.push(b'@');
        }
        s.extend_from_slice(&self.host);
        if let Some(p) = &self.port {
            s.push(b':');
            s.extend_from_slice(p);
        }
        s.extend_from_slice(&self.path);
        if let Some(q) = &self.query {
            s.push(b'?');
            s.extend_from_slice(q);
        }
        if let Some(f) = &self.fragment {
            s.push(b'#');
            s.extend_from_slice(f);
        }
        s
    }

    /// `ssh://host/~user/repo` names a path relative to a home directory.
    fn strip_tilde(&mut self) {
        let sshlike = matches!(
            self.scheme.as_slice(),
            b"ssh" | b"git" | b"git+ssh" | b"ssh+git"
        );
        if sshlike && self.path.starts_with(b"/~") {
            self.path.remove(0);
        }
    }
}

fn parse(url: &[u8]) -> Result<Parts, Error> {
    let mut parts = if url.windows(3).any(|w| w == b"://") {
        let first = normalize(url)?;
        normalize(&percent_decode(&first.serialize()))?
    } else {
        parse_scp(url)?
    };
    parts.strip_tilde();
    Ok(parts)
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~".contains(&b)
}

fn is_reserved(b: u8) -> bool {
    b":/?#[]@!$&'()*+,;=".contains(&b)
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// Normalize the percent escapes of a user, password, path, query or
/// fragment: unreserved characters are decoded, other escapes upper-cased
/// and characters that are neither reserved nor unreserved escaped.
fn normalize_escapes(s: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'%' {
            let (Some(h), Some(l)) = (
                s.get(i + 1).copied().and_then(hex),
                s.get(i + 2).copied().and_then(hex),
            ) else {
                return Err(Error::Invalid(msg::BAD_ESCAPE));
            };
            let v = h << 4 | l;
            if is_unreserved(v) {
                out.push(v);
            } else {
                out.extend_from_slice(format!("%{v:02X}").as_bytes());
            }
            i += 3;
            continue;
        }
        if is_unreserved(b) || is_reserved(b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
        i += 1;
    }
    Ok(out)
}

/// Resolve `.` and `..` segments of a path starting with `/`.
fn resolve_dots(path: &[u8]) -> Result<Vec<u8>, Error> {
    let mut segs: Vec<&[u8]> = Vec::new();
    let all: Vec<&[u8]> = path[1..].split(|&b| b == b'/').collect();
    let n = all.len();
    let mut trailing = false;
    for (i, seg) in all.into_iter().enumerate() {
        let last = i + 1 == n;
        match seg {
            b"." => trailing = last,
            b".." => {
                if segs.pop().is_none() {
                    return Err(Error::Invalid(msg::BAD_DOTDOT));
                }
                trailing = last;
            }
            s => {
                segs.push(s);
                trailing = false;
            }
        }
    }
    let mut out = vec![b'/'];
    out.extend_from_slice(&segs.join(&b'/'));
    if trailing && !out.ends_with(b"/") {
        out.push(b'/');
    }
    Ok(out)
}

fn normalize_path(path: &[u8]) -> Result<Vec<u8>, Error> {
    let path = if path.is_empty() { b"/" } else { path };
    resolve_dots(&normalize_escapes(path)?)
}

fn split_userinfo(userinfo: &[u8]) -> Result<(Vec<u8>, Option<Vec<u8>>), Error> {
    let userinfo = normalize_escapes(userinfo)?;
    Ok(match userinfo.iter().position(|&b| b == b':') {
        Some(i) => (userinfo[..i].to_vec(), Some(userinfo[i + 1..].to_vec())),
        None => (userinfo, None),
    })
}

fn check_port(port: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    if port.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if !port.iter().all(u8::is_ascii_digit) {
        return Err(Error::Invalid(msg::BAD_PORT));
    }
    let digits = port.iter().position(|&b| b != b'0').map_or(&b""[..], |i| &port[i..]);
    if digits.is_empty() || digits.len() > 5 {
        return Err(Error::Invalid(msg::BAD_PORT));
    }
    let value: u32 = std::str::from_utf8(digits)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if value == 0 || value > 65535 {
        return Err(Error::Invalid(msg::BAD_PORT));
    }
    Ok(Some(digits.to_vec()))
}

/// Normalize the host of a `scheme://` URL: decode escapes, lower-case it
/// and reject characters that cannot appear in a host name.
fn normalize_host(host: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(host.len());
    let mut i = 0;
    while i < host.len() {
        let mut b = host[i];
        if b == b'%' {
            match (
                host.get(i + 1).copied().and_then(hex),
                host.get(i + 2).copied().and_then(hex),
            ) {
                (Some(h), Some(l)) => {
                    b = h << 4 | l;
                    i += 2;
                    if !(b.is_ascii_alphanumeric() || b"-._".contains(&b)) {
                        return Err(Error::Invalid(msg::BAD_HOST));
                    }
                }
                _ => return Err(Error::Invalid(msg::BAD_HOST)),
            }
        } else if !(b.is_ascii_alphanumeric() || b"-._:[]".contains(&b)) {
            return Err(Error::Invalid(msg::BAD_HOST));
        }
        out.push(b.to_ascii_lowercase());
        i += 1;
    }
    Ok(out)
}

/// Normalize a `scheme://...` URL.
fn normalize(url: &[u8]) -> Result<Parts, Error> {
    let sep = url
        .windows(3)
        .position(|w| w == b"://")
        .ok_or(Error::Invalid(msg::BAD_SCHEME))?;
    let scheme = &url[..sep];
    let valid_scheme = scheme.first().is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b"+.-".contains(&b));
    if !valid_scheme {
        return Err(Error::Invalid(msg::BAD_SCHEME));
    }
    let mut parts = Parts {
        scheme: scheme.to_ascii_lowercase(),
        ..Parts::default()
    };
    let rest = &url[sep + 3..];
    let auth_end = rest
        .iter()
        .position(|b| b"/?#".contains(b))
        .unwrap_or(rest.len());
    let auth = &rest[..auth_end];
    let hostport = match auth.iter().position(|&b| b == b'@') {
        Some(at) => {
            let (user, password) = split_userinfo(&auth[..at])?;
            parts.user = Some(user);
            parts.password = password;
            &auth[at + 1..]
        }
        None => auth,
    };
    let colon = if hostport.first() == Some(&b'[') {
        let close = hostport.iter().position(|&b| b == b']').unwrap_or(0);
        hostport[close..]
            .iter()
            .rposition(|&b| b == b':')
            .map(|i| close + i)
    } else {
        hostport.iter().rposition(|&b| b == b':')
    };
    let (host, port) = match colon {
        Some(i) => (&hostport[..i], Some(&hostport[i + 1..])),
        None => (hostport, None),
    };
    parts.host = normalize_host(host)?;
    if let Some(p) = port {
        parts.port = check_port(p)?;
    }
    if parts.host.is_empty() && parts.scheme != b"file" {
        return Err(Error::Invalid(msg::NO_HOST));
    }
    let rest = &rest[auth_end..];
    let path_end = rest
        .iter()
        .position(|b| b"?#".contains(b))
        .unwrap_or(rest.len());
    parts.path = normalize_path(&rest[..path_end])?;
    let mut rest = &rest[path_end..];
    if rest.first() == Some(&b'?') {
        let end = rest.iter().position(|&b| b == b'#').unwrap_or(rest.len());
        parts.query = Some(normalize_escapes(&rest[1..end])?);
        rest = &rest[end..];
    }
    if rest.first() == Some(&b'#') {
        parts.fragment = Some(normalize_escapes(&rest[1..])?);
    }
    Ok(parts)
}

fn valid_scp_host(host: &[u8]) -> bool {
    host.iter()
        .all(|&b| b.is_ascii_alphanumeric() || b"-._:".contains(&b))
}

/// Parse the scp-like `[user@]host:path`.
fn parse_scp(url: &[u8]) -> Result<Parts, Error> {
    // The first colon outside brackets ends the host; a slash before it
    // makes this a local path.
    let mut depth = 0;
    let mut first_colon = None;
    for (i, &b) in url.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' if depth > 0 => depth -= 1,
            b':' if depth == 0 => {
                first_colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let bracket_host = |s: &[u8]| s.first() == Some(&b'[') && s.contains(&b']');
    let after_at = url
        .iter()
        .position(|&b| b == b'@')
        .map_or(url, |i| &url[i + 1..]);
    let first_colon = match first_colon {
        Some(c) => c,
        None if bracket_host(url) || bracket_host(after_at) => url.len(),
        None => return Err(Error::NotUrl),
    };
    if url[..first_colon].contains(&b'/') {
        return Err(Error::NotUrl);
    }
    let mut parts = Parts {
        scheme: b"ssh".to_vec(),
        ..Parts::default()
    };
    let at = url
        .iter()
        .position(|&b| b == b'@' || b == b'/')
        .filter(|&i| url[i] == b'@');
    let hp = match at {
        Some(at) => {
            let (user, password) = split_userinfo(&url[..at])?;
            parts.user = Some(user);
            parts.password = password;
            &url[at + 1..]
        }
        None => url,
    };
    let (host, path): (Vec<u8>, &[u8]) = if hp.first() == Some(&b'[') {
        let Some(close) = hp.iter().position(|&b| b == b']') else {
            return Err(Error::NotUrl);
        };
        let inner = &hp[1..close];
        let mut host = if inner.iter().filter(|&&b| b == b':').count() >= 2 {
            hp[..=close].to_vec()
        } else {
            inner.to_vec()
        };
        let path: &[u8] = if at.is_none() {
            hp.get(close + 2..).unwrap_or_default()
        } else {
            match hp[close + 1..].iter().position(|&b| b == b':') {
                Some(i) => {
                    host.extend_from_slice(&hp[close + 1..close + 1 + i]);
                    &hp[close + 2 + i..]
                }
                None => b"",
            }
        };
        let bracketed = host.first() == Some(&b'[');
        let check = if bracketed {
            &host[1..host.iter().position(|&b| b == b']').unwrap_or(host.len())]
        } else {
            &host[..]
        };
        if !valid_scp_host(check) {
            return Err(Error::Invalid(msg::BAD_HOST));
        }
        (host, path)
    } else {
        let (host, path) = match hp.iter().position(|&b| b == b':') {
            Some(i) => (&hp[..i], &hp[i + 1..]),
            None => (hp, &b""[..]),
        };
        if !valid_scp_host(host) {
            return Err(Error::Invalid(msg::BAD_HOST));
        }
        (host.to_vec(), path)
    };
    let (host, port) = match host.iter().position(|&b| b == b':') {
        Some(i) if host.first() != Some(&b'[') => {
            (host[..i].to_vec(), check_port(&host[i + 1..])?)
        }
        _ => (host, None),
    };
    if host.is_empty() {
        return Err(Error::Invalid(msg::NO_HOST));
    }
    parts.host = host.to_ascii_lowercase();
    parts.port = port;
    let end = path
        .iter()
        .position(|b| b"?#".contains(b))
        .unwrap_or(path.len());
    let path = &path[..end];
    let mut full = Vec::with_capacity(path.len() + 1);
    if path.first() != Some(&b'/') {
        full.push(b'/');
    }
    full.extend_from_slice(path);
    parts.path = normalize_path(&full)?;
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(url: &str) -> Parts {
        parse(url.as_bytes()).unwrap()
    }

    #[test]
    fn url_form() {
        let p = get("HTTPS://u:pw@Example.COM:0443/a/./b/../c?q#f");
        assert_eq!(p.scheme, b"https");
        assert_eq!(p.user.as_deref(), Some(&b"u"[..]));
        assert_eq!(p.password.as_deref(), Some(&b"pw"[..]));
        assert_eq!(p.host, b"example.com");
        assert_eq!(p.port.as_deref(), Some(&b"443"[..]));
        assert_eq!(p.path, b"/a/c");
        assert_eq!(get("ssh://h/~me/x").path, b"~me/x");
        assert_eq!(get("https://h/~me").path, b"/~me");
        assert_eq!(get("file://").path, b"/");
        assert_eq!(get("https://h/a b").path, b"/a%20b");
        assert_eq!(get("https://h/%2f%7e").path, b"//~");
        assert_eq!(get("https://u%3fx@h/").host, b"u");
    }

    #[test]
    fn scp_form() {
        let p = get("me@[srv:22]:~me/r");
        assert_eq!((p.host.as_slice(), p.port.as_deref()), (&b"srv"[..], Some(&b"22"[..])));
        assert_eq!(p.path, b"~me/r");
        assert_eq!(get("[::1]:r").host, b"[::1]");
        assert_eq!(get("h:r").path, b"/r");
        assert_eq!(get("h:/p@q").path, b"/p@q");
        assert_eq!(get("h:a%3fb").path, b"/a%3Fb");
    }

    #[test]
    fn errors() {
        assert_eq!(parse(b"a/b:c"), Err(Error::NotUrl));
        assert_eq!(parse(b"plain"), Err(Error::NotUrl));
        assert_eq!(parse(b"https://"), Err(Error::Invalid(msg::NO_HOST)));
        assert_eq!(parse(b"https://h:0/"), Err(Error::Invalid(msg::BAD_PORT)));
        assert_eq!(parse(b"https://h/../x"), Err(Error::Invalid(msg::BAD_DOTDOT)));
        assert_eq!(parse(b"9x://h/"), Err(Error::Invalid(msg::BAD_SCHEME)));
        assert_eq!(parse(b"https://h/%"), Err(Error::Invalid(msg::BAD_ESCAPE)));
        assert_eq!(parse(b"a b:c"), Err(Error::Invalid(msg::BAD_HOST)));
    }
}
