// SPDX-License-Identifier: MIT
//! Submodule URLs: the safety check and relative URL resolution.

use bstr::{BString, ByteSlice};

/// Schemes whose URLs are checked strictly (they are handed to the HTTP
/// and FTP transports, which parse host, port and percent-escapes).
const STRICT_SCHEMES: &[&[u8]] = &[b"http", b"https", b"ftp", b"ftps"];

/// Whether a value would be taken for an option when passed to a command.
pub fn looks_like_option(value: &[u8]) -> bool {
    value.first() == Some(&b'-')
}

/// Whether `url` is relative to the superproject's remote: it begins with
/// `./` or `../` (a backslash also counts as separator).
pub fn is_relative_url(url: &[u8]) -> bool {
    url.starts_with(b"./")
        || url.starts_with(b"../")
        || url.starts_with(b".\\")
        || url.starts_with(b"..\\")
}

/// Whether `url` is acceptable as `submodule.<name>.url`.
///
/// Rejected are values that look like options; relative URLs whose leading
/// `../` steps are followed by `/` or `:` (resolved against a remote they
/// could produce an empty host or a bare scheme); and URLs that contain a
/// newline, raw or percent-encoded, where the transport could pass it on
/// (relative URLs, `git://`, and the HTTP/FTP family). URLs of the HTTP/FTP
/// family, also behind a `<transport>::` prefix, must in addition have a
/// host, a valid port and valid percent-escapes.
pub fn check_url(url: &[u8]) -> bool {
    if looks_like_option(url) {
        return false;
    }
    if is_relative_url(url) {
        let mut rest = url;
        let mut went_up = false;
        loop {
            if rest.starts_with(b"./") || rest.starts_with(b".\\") {
                rest = &rest[2..];
            } else if rest.starts_with(b"../") || rest.starts_with(b"..\\") {
                rest = &rest[3..];
                went_up = true;
            } else {
                break;
            }
        }
        if went_up && matches!(rest.first(), Some(b'/' | b':')) {
            return false;
        }
        return !has_newline_lenient(url);
    }
    if let Some(pos) = url.find(b"::") {
        if STRICT_SCHEMES.contains(&&url[..pos]) {
            return check_strict(&url[pos + 2..]);
        }
    }
    if let Some(pos) = url.find(b"://") {
        let scheme = &url[..pos];
        if STRICT_SCHEMES.contains(&scheme) {
            return check_strict(url);
        }
        if scheme == b"git" {
            return !has_newline_lenient(url);
        }
    }
    true
}

/// Decode `%XX` escapes where they are valid and report a newline.
fn has_newline_lenient(s: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'\n' => return true,
            b'%' if i + 2 < s.len() => {
                if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                    if h * 16 + l == b'\n' {
                        return true;
                    }
                    i += 3;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Decode `%XX` escapes strictly: a `%` not followed by two hex digits is
/// an error.
fn decode_strict(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let h = hex(*s.get(i + 1)?)?;
            let l = hex(*s.get(i + 2)?)?;
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

/// `<scheme>://[<user>[:<password>]@]<host>[:<port>][/<path>]` with a
/// non-empty scheme and host, a port in 1..=65535 if given, valid
/// percent-escapes and no newline anywhere.
fn check_strict(url: &[u8]) -> bool {
    let Some(pos) = url.find(b"://") else {
        return false;
    };
    let scheme = &url[..pos];
    if scheme.is_empty()
        || !scheme
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
    {
        return false;
    }
    let rest = &url[pos + 3..];
    let (authority, path) = match rest.find_byte(b'/') {
        Some(i) => rest.split_at(i),
        None => (rest, &b""[..]),
    };
    let host_port = match authority.find_byte(b'@') {
        Some(i) => {
            let (user, hp) = (&authority[..i], &authority[i + 1..]);
            if hp.contains(&b'@') || !decodes_cleanly(user) {
                return false;
            }
            hp
        }
        None => authority,
    };
    let (host, port) = split_port(host_port);
    if host.is_empty()
        || host.iter().any(u8::is_ascii_whitespace)
        || !decodes_cleanly(host)
        || !decodes_cleanly(path)
    {
        return false;
    }
    match port {
        None => true,
        Some([]) => true,
        Some(p) => {
            p.iter().all(u8::is_ascii_digit)
                && p.to_str()
                    .ok()
                    .and_then(|s| s.trim_start_matches('0').parse::<u32>().ok())
                    .is_some_and(|n| (1..=65535).contains(&n))
        }
    }
}

fn decodes_cleanly(s: &[u8]) -> bool {
    decode_strict(s).is_some_and(|d| !d.contains(&b'\n'))
}

/// Split `host[:port]`; a bracketed IPv6 host keeps its colons.
fn split_port(hp: &[u8]) -> (&[u8], Option<&[u8]>) {
    if hp.first() == Some(&b'[') {
        if let Some(close) = hp.find_byte(b']') {
            let host = &hp[..=close];
            let after = &hp[close + 1..];
            return match after.first() {
                Some(b':') => (host, Some(&after[1..])),
                _ => (host, None),
            };
        }
        return (hp, None);
    }
    match hp.rfind_byte(b':') {
        Some(i) => (&hp[..i], Some(&hp[i + 1..])),
        None => (hp, None),
    }
}

/// [`resolve_relative_url`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelativeUrlError {
    /// The remote URL is empty.
    EmptyRemote,
    /// A `../` step went above the start of a relative remote (the remote
    /// as far as it was shortened).
    CannotStrip(BString),
}

impl std::fmt::Display for RelativeUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelativeUrlError::EmptyRemote => f.write_str(crate::messages::EMPTY_REMOTE_URL),
            RelativeUrlError::CannotStrip(u) => {
                f.write_str(&crate::messages::cannot_strip_component(u.as_bstr()))
            }
        }
    }
}

impl std::error::Error for RelativeUrlError {}

/// Whether `url` is a local path (no `:` before the first `/`), as opposed
/// to a URL or scp-like `host:path`.
fn is_local_path(url: &[u8]) -> bool {
    match url.find_byte(b':') {
        None => true,
        Some(colon) => url[..colon].contains(&b'/'),
    }
}

/// Resolve the submodule URL `url` (usually starting with `./` or `../`)
/// against `remote`, the URL of the superproject's remote.
///
/// Each leading `../` removes the last component of the remote: the part
/// after the last `/`, or for URLs and scp-like addresses also after the
/// last `:` (which then joins the rest instead of `/`). A URL remote that
/// runs out of components becomes `.`; a relative remote (a local path that
/// is not absolute) cannot, which is an error. Leading `./` steps are
/// dropped. A leading `./` of the result is removed. If the remote is a
/// relative path and `up_path` is given, `up_path` is prepended (it leads
/// from the submodule's directory back to the superproject's).
pub fn resolve_relative_url(
    up_path: Option<&[u8]>,
    remote: &[u8],
    url: &[u8],
) -> Result<BString, RelativeUrlError> {
    if remote.is_empty() {
        return Err(RelativeUrlError::EmptyRemote);
    }
    let relative = is_local_path(remote) && !remote.starts_with(b"/");
    let mut remote: Vec<u8> = remote.strip_suffix(b"/").unwrap_or(remote).to_vec();
    if relative && !remote.starts_with(b"./") && !remote.starts_with(b"../") {
        remote.splice(0..0, *b"./");
    }
    let mut url = url;
    let mut sep = b'/';
    loop {
        if let Some(rest) = url.strip_prefix(b"../") {
            url = rest;
            let cut = if relative {
                remote.rfind_byte(b'/')
            } else {
                remote.rfind_byteset(b"/:")
            };
            match cut {
                Some(i) => {
                    sep = remote[i];
                    remote.truncate(i);
                }
                None if relative => {
                    return Err(RelativeUrlError::CannotStrip(remote.into()));
                }
                None => remote = b".".to_vec(),
            }
        } else if let Some(rest) = url.strip_prefix(b"./") {
            url = rest;
        } else {
            break;
        }
    }
    let url = url.strip_suffix(b"/").unwrap_or(url);
    let mut out = remote;
    out.push(sep);
    out.extend_from_slice(url);
    if out.starts_with(b"./") {
        out.drain(..2);
    }
    if relative {
        if let Some(up) = up_path {
            out.splice(0..0, up.iter().copied());
        }
    }
    Ok(out.into())
}
