// SPDX-License-Identifier: MIT
//! Classification of a remote location into local path, SSH target or
//! another scheme, and the host/port/path split git applies before it runs
//! `ssh`.
//!
//! This is the part of URL handling the transports need; full URL and
//! remote configuration handling (`insteadOf`, remotes, refspecs) is #159.
//!
//! Rules (`Documentation/urls.adoc` and observations of git 2.56):
//!
//! - `<scheme>://...`: `ssh`, `git+ssh` and `ssh+git` are SSH, `file` is
//!   local, everything else is left to other transports.
//! - Otherwise an existing local path is local; `[user@]host:path` is SSH
//!   if there is no `/` before the first colon (a leading `[...]` is
//!   skipped when looking for the colon); anything else is a local path.
//! - `ssh://` URLs are percent-decoded; the path keeps its leading `/`
//!   except before `~`. scp-style paths are used as written, minus a `/`
//!   before `~`.
//! - The port is split off at the only colon of the host part (or after a
//!   bracketed `[host]`). It must be a decimal number from 0 to 65535,
//!   optionally with a sign; otherwise it stays part of the host. An empty
//!   port is dropped.
//! - Host names and scp-style paths that begin with `-` are refused, as
//!   `ssh` would read them as options.

use bstr::{BString, ByteSlice};

use crate::{Error, Result, messages};

/// Where a remote repository lives, as far as the transport is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remote {
    /// A local path or `file://` URL; `path` is passed to the service.
    Local {
        /// The path.
        path: BString,
    },
    /// A repository reached through `ssh`.
    Ssh(SshTarget),
    /// A URL for another transport (`http`, `https`, `git`, ...).
    Other {
        /// The scheme before `://`.
        scheme: String,
    },
}

/// The pieces of an SSH location as they are passed to `ssh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    /// `[user@]host` as given to ssh.
    pub host: BString,
    /// The port, exactly as written in the URL.
    pub port: Option<BString>,
    /// The repository path for the remote command.
    pub path: BString,
}

/// Classify `url`. `local_exists` says whether a path exists locally (an
/// existing `foo:bar` directory is not an SSH location).
pub fn parse_remote(url: &str, local_exists: &dyn Fn(&str) -> bool) -> Result<Remote> {
    if let Some(i) = url.find("://") {
        let scheme = &url[..i];
        if is_scheme(scheme) {
            let rest = &url[i + 3..];
            return match scheme {
                "ssh" | "git+ssh" | "ssh+git" => parse_ssh_url(rest).map(Remote::Ssh),
                "file" => Ok(Remote::Local {
                    path: percent_decode(rest.as_bytes()),
                }),
                _ => Ok(Remote::Other {
                    scheme: scheme.to_string(),
                }),
            };
        }
    }
    if local_exists(url) {
        return Ok(Remote::Local { path: url.into() });
    }
    match scp_colon(url.as_bytes()) {
        Some(colon) => parse_scp(url.as_bytes(), colon).map(Remote::Ssh),
        None => Ok(Remote::Local { path: url.into() }),
    }
}

fn is_scheme(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphabetic()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(c))
}

/// The colon of a scp-style `host:path`, if `url` is one.
fn scp_colon(url: &[u8]) -> Option<usize> {
    let start = if url.first() == Some(&b'[') {
        url.find_byte(b']').map(|i| i + 1).unwrap_or(0)
    } else {
        0
    };
    let colon = start + url[start..].find_byte(b':')?;
    if url[..colon].contains(&b'/') {
        return None;
    }
    Some(colon)
}

fn parse_scp(url: &[u8], colon: usize) -> Result<SshTarget> {
    let (host, port) = split_host_port(&url[..colon]);
    check_host(&host)?;
    let mut path = &url[colon + 1..];
    if path.starts_with(b"/~") {
        path = &path[1..];
    }
    if path.first() == Some(&b'-') {
        return Err(Error::Refused(messages::strange_pathname(
            &path.to_str_lossy(),
        )));
    }
    Ok(SshTarget {
        host,
        port,
        path: path.into(),
    })
}

fn parse_ssh_url(rest: &str) -> Result<SshTarget> {
    let rest = rest.as_bytes();
    let Some(slash) = rest.find_byte(b'/') else {
        return Err(Error::Refused(messages::NO_PATH.into()));
    };
    let hostpart = percent_decode(&rest[..slash]);
    let mut path = percent_decode(&rest[slash..]);
    if path.starts_with(b"/~") {
        path.remove(0);
    }
    let (host, port) = split_host_port(&hostpart);
    check_host(&host)?;
    Ok(SshTarget { host, port, path })
}

fn check_host(host: &[u8]) -> Result<()> {
    if host.first() == Some(&b'-') {
        return Err(Error::Refused(messages::strange_hostname(
            &host.to_str_lossy(),
        )));
    }
    Ok(())
}

/// Split `[user@]host[:port]` (with optional brackets around the host or
/// around `user@host`).
pub fn split_host_port(s: &[u8]) -> (BString, Option<BString>) {
    if let Some(lb) = s.find_byte(b'[') {
        if let Some(rel) = s[lb..].find_byte(b']') {
            let rb = lb + rel;
            let mut host: Vec<u8> = s[..lb].to_vec();
            host.extend_from_slice(&s[lb + 1..rb]);
            let after = &s[rb + 1..];
            if after.is_empty() {
                return one_colon_split(&host);
            }
            if let Some(cand) = after.strip_prefix(b":") {
                if cand.is_empty() {
                    return (host.into(), None);
                }
                if valid_port(cand) {
                    return (host.into(), Some(cand.into()));
                }
            }
            return (s.into(), None);
        }
    }
    one_colon_split(s)
}

fn one_colon_split(s: &[u8]) -> (BString, Option<BString>) {
    if s.iter().filter(|&&b| b == b':').count() != 1 {
        return (s.into(), None);
    }
    let colon = s.find_byte(b':').unwrap_or(0);
    let cand = &s[colon + 1..];
    if cand.is_empty() {
        (s[..colon].into(), None)
    } else if valid_port(cand) {
        (s[..colon].into(), Some(cand.into()))
    } else {
        (s.into(), None)
    }
}

/// A decimal number 0..=65535 with optional leading blanks and sign,
/// nothing after it.
fn valid_port(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || (b'\t'..=b'\r').contains(&s[i])) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let digits = &s[i..];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let mut v: u64 = 0;
    for d in digits {
        v = v.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    if neg {
        return v == 0;
    }
    v <= 65535
}

/// Decode `%XX` escapes; invalid escapes are kept.
pub fn percent_decode(s: &[u8]) -> BString {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out.into()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(url: &str) -> (String, Option<String>, String) {
        match parse_remote(url, &|_| false).unwrap() {
            Remote::Ssh(t) => (
                t.host.to_string(),
                t.port.map(|p| p.to_string()),
                t.path.to_string(),
            ),
            other => panic!("{url}: {other:?}"),
        }
    }

    fn s(h: &str, p: Option<&str>, path: &str) -> (String, Option<String>, String) {
        (h.into(), p.map(Into::into), path.into())
    }

    #[test]
    fn scp_forms() {
        assert_eq!(ssh("myhost:src"), s("myhost", None, "src"));
        assert_eq!(ssh("[myhost:123]:src"), s("myhost", Some("123"), "src"));
        assert_eq!(ssh("c:temp"), s("c", None, "temp"));
        assert_eq!(ssh("[::1]:rep"), s("::1", None, "rep"));
        assert_eq!(ssh("host:/~repo"), s("host", None, "~repo"));
        assert_eq!(ssh("[::1]:/~repo"), s("::1", None, "~repo"));
        assert_eq!(ssh("[user@host]:repo"), s("user@host", None, "repo"));
        assert_eq!(ssh("[host:22:33]:x"), s("host:22:33", None, "x"));
        assert_eq!(ssh("user@host:~/repo"), s("user@host", None, "~/repo"));
    }

    #[test]
    fn not_ssh() {
        for url in ["foo/bar:baz", "[foo]bar/baz:qux", "[foo/bar]:baz", "plain"] {
            assert_eq!(
                parse_remote(url, &|_| false).unwrap(),
                Remote::Local { path: url.into() },
                "{url}"
            );
        }
        assert_eq!(
            parse_remote("foo:bar", &|_| true).unwrap(),
            Remote::Local {
                path: "foo:bar".into()
            }
        );
        assert_eq!(
            parse_remote("https://example.com/x", &|_| false).unwrap(),
            Remote::Other {
                scheme: "https".into()
            }
        );
        assert_eq!(
            parse_remote("file:///tmp/a%20b", &|_| false).unwrap(),
            Remote::Local {
                path: "/tmp/a b".into()
            }
        );
    }

    #[test]
    fn ssh_urls() {
        assert_eq!(
            ssh("ssh://host.xz/home/user/repo"),
            s("host.xz", None, "/home/user/repo")
        );
        assert_eq!(ssh("ssh://host.xz:/~repo"), s("host.xz", None, "~repo"));
        assert_eq!(
            ssh("ssh://host.xz:22/~repo"),
            s("host.xz", Some("22"), "~repo")
        );
        for (tuah, host) in [
            ("::1", "::1"),
            ("[::1]", "::1"),
            ("[::1]:", "::1"),
            ("user@::1", "user@::1"),
            ("user@[::1]", "user@::1"),
            ("user@[::1]:", "user@::1"),
            ("[user@::1]", "user@::1"),
            ("[user@::1]:", "user@::1"),
        ] {
            assert_eq!(
                ssh(&format!("ssh://{tuah}/home/user/repo")),
                s(host, None, "/home/user/repo"),
                "{tuah}"
            );
        }
        for (tuah, host) in [
            ("[::1]", "::1"),
            ("user@[::1]", "user@::1"),
            ("[user@::1]", "user@::1"),
        ] {
            assert_eq!(
                ssh(&format!("ssh://{tuah}:22/~repo")),
                s(host, Some("22"), "~repo"),
                "{tuah}"
            );
        }
        assert_eq!(ssh("ssh://host:abc/r"), s("host:abc", None, "/r"));
        assert_eq!(ssh("ssh://host:-22/r"), s("host:-22", None, "/r"));
        assert_eq!(ssh("ssh://host:99999/r"), s("host:99999", None, "/r"));
        assert_eq!(ssh("ssh://host:+22/r"), s("host", Some("+22"), "/r"));
        assert_eq!(ssh("ssh://host:065535/r"), s("host", Some("065535"), "/r"));
        assert_eq!(ssh("ssh://user:pw@host/r"), s("user:pw@host", None, "/r"));
        assert_eq!(
            ssh("ssh://us%40er@ho%41st/r%20epo"),
            s("us@er@hoAst", None, "/r epo")
        );
        assert_eq!(ssh("git+ssh://host/repo"), s("host", None, "/repo"));
        assert_eq!(ssh("ssh+git://host/repo"), s("host", None, "/repo"));
        assert_eq!(ssh("ssh://host/-repo"), s("host", None, "/-repo"));
        assert_eq!(
            ssh("ssh://[fe80::1%25eth0]:22/repo"),
            s("fe80::1%eth0", Some("22"), "/repo")
        );
    }

    #[test]
    fn refused() {
        let msg = |url: &str| parse_remote(url, &|_| false).unwrap_err().to_string();
        assert_eq!(
            msg("ssh://-oProxyCommand=x/repo"),
            "strange hostname '-oProxyCommand=x' blocked"
        );
        assert_eq!(msg("[-x]:repo"), "strange hostname '-x' blocked");
        assert_eq!(
            msg("ssh://-user@host/repo"),
            "strange hostname '-user@host' blocked"
        );
        assert_eq!(msg("host:-repo"), "strange pathname '-repo' blocked");
        assert_eq!(msg("ssh://host"), messages::NO_PATH);
    }
}
