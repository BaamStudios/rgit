// SPDX-License-Identifier: MIT
//! Checks of `.gitmodules` blobs (fsck-msgids `gitmodulesParse`,
//! `gitmodulesName`, `gitmodulesPath`, `gitmodulesUrl`,
//! `gitmodulesUpdate`), shared by `fsck`, `index-pack --strict` and the
//! object checks of `fetch` (`transfer.fsckObjects`).

use rgit_object::fsck::MsgId;

use crate::messages::fsck as msg;

/// Whether a submodule name may be used: not empty and without a `..`
/// path component (`/` and `\` both separate components).
fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && !name.split(|&b| b == b'/' || b == b'\\').any(|c| c == b"..")
}

/// `%XX` decoding.
fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            let hex = std::str::from_utf8(&s[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// Whether a submodule URL is acceptable: no leading `-`, and URLs of
/// the curl protocols must have a valid authority (a numeric port) and
/// no newline once decoded.
fn valid_url(url: &[u8]) -> bool {
    if url.first() == Some(&b'-') {
        return false;
    }
    if percent_decode(url).contains(&b'\n') {
        return false;
    }
    let Some(i) = url.windows(3).position(|w| w == b"://") else {
        return true;
    };
    let scheme = url[..i].to_ascii_lowercase();
    if !matches!(scheme.as_slice(), b"http" | b"https" | b"ftp" | b"ftps") {
        return true;
    }
    let rest = &url[i + 3..];
    let end = rest
        .iter()
        .position(|&b| b == b'/' || b == b'?' || b == b'#')
        .unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = match authority.iter().rposition(|&b| b == b'@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    // `[v6]:port` or `host:port`.
    let port = if host_port.first() == Some(&b'[') {
        match host_port.iter().position(|&b| b == b']') {
            Some(close) => host_port[close + 1..].strip_prefix(b":"),
            None => return false,
        }
    } else {
        host_port
            .iter()
            .position(|&b| b == b':')
            .map(|c| &host_port[c + 1..])
    };
    match port {
        None => !host_port.is_empty(),
        Some(p) => p.iter().all(u8::is_ascii_digit),
    }
}

/// The findings for `.gitmodules` content `data`.
pub fn check(data: &[u8]) -> Vec<(MsgId, String)> {
    let mut out = Vec::new();
    let file = match rgit_config::File::parse(data.to_vec()) {
        Ok(f) => f,
        Err(_) => {
            out.push((MsgId::GitmodulesParse, msg::GITMODULES_PARSE.to_string()));
            return out;
        }
    };
    for (name, entry) in file.entries() {
        let Some(rest) = name.strip_prefix(b"submodule.") else {
            continue;
        };
        let Some(dot) = rest.iter().rposition(|&b| b == b'.') else {
            continue;
        };
        let (sub, key) = (&rest[..dot], &rest[dot + 1..]);
        let shown = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
        if !valid_name(sub) {
            out.push((MsgId::GitmodulesName, msg::disallowed_name(&shown(sub))));
        }
        let Some(value) = entry.value.as_ref() else {
            continue;
        };
        let value: &[u8] = value.as_ref();
        match key {
            b"url" if !valid_url(value) => {
                out.push((MsgId::GitmodulesUrl, msg::disallowed_url(&shown(value))));
            }
            b"path" if value.first() == Some(&b'-') => {
                out.push((MsgId::GitmodulesPath, msg::disallowed_path(&shown(value))));
            }
            b"update" if value.first() == Some(&b'!') => {
                out.push((
                    MsgId::GitmodulesUpdate,
                    msg::disallowed_update(&shown(value)),
                ));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_urls() {
        assert!(valid_name(b"a"));
        assert!(!valid_name(b"../x"));
        assert!(!valid_name(b"a\\..\\b"));
        assert!(valid_url(b"https://example.com:8080/x"));
        assert!(!valid_url(b"http://example.com:test/"));
        assert!(!valid_url(b"https://e.com/%0atest"));
        assert!(!valid_url(b"-u"));
        assert!(valid_url(b"./-u"));
    }
}
