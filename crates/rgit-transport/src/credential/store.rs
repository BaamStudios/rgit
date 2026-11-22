// SPDX-License-Identifier: MIT
//! The file format of `git credential-store`
//! (`Documentation/git-credential-store.adoc`, "STORAGE FORMAT"): one
//! credential per line, written as `<protocol>://<user>:<password>@<host>[/<path>]`.
//!
//! Details observed with git 2.56:
//!
//! - User name, password and host (including a `:port`) are
//!   percent-encoded except for ASCII letters, digits and `-._~`; the path
//!   keeps the RFC 3986 reserved characters as well (`/`, `:`, `@`, `?`,
//!   `#`, `[`, `]`, `!`, `$`, `&`, `'`, `(`, `)`, `*`, `+`, `,`, `;`,
//!   `=`). Hex digits are lower case.
//! - A line is used only when it has a scheme, a user name and a password
//!   (both may be empty: `https://:@host`), and a host or a path. Other
//!   lines (empty, comments, a trailing carriage return in the host) never
//!   match and are kept as they are when the file is rewritten.
//! - A stored credential goes to the top of the file.

use bstr::{BString, ByteSlice};

use super::{Credential, split_authority};

fn push_encoded(out: &mut Vec<u8>, v: &[u8], keep: impl Fn(u8) -> bool) {
    for &b in v {
        if keep(b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02x}").as_bytes());
        }
    }
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~".contains(&b)
}

fn path_char(b: u8) -> bool {
    unreserved(b) || b":/?#[]@!$&'()*+,;=".contains(&b)
}

/// The line for `cred`, or `None` if it lacks protocol, host, user name or
/// password (such credentials are not stored).
pub fn format_line(cred: &Credential) -> Option<BString> {
    let (Some(protocol), Some(host), Some(user), Some(pass)) =
        (&cred.protocol, &cred.host, &cred.username, &cred.password)
    else {
        return None;
    };
    let mut out = Vec::new();
    out.extend_from_slice(protocol);
    out.extend_from_slice(b"://");
    push_encoded(&mut out, user, unreserved);
    out.push(b':');
    push_encoded(&mut out, pass, unreserved);
    out.push(b'@');
    push_encoded(&mut out, host, unreserved);
    if let Some(path) = &cred.path {
        out.push(b'/');
        push_encoded(&mut out, path, path_char);
    }
    Some(out.into())
}

/// Parse one line of the file (without its `\n`). `None` for a line that
/// is not a usable credential; nothing is printed.
pub fn parse_line(line: &[u8]) -> Option<Credential> {
    let sep = line.find(b"://").filter(|&i| i > 0)?;
    let parts = split_authority(&line[sep + 3..]);
    let (Some(username), Some(password)) = (parts.username, parts.password) else {
        return None;
    };
    if parts.host.is_empty() && parts.path.is_none() {
        return None;
    }
    if [&username, &password, &parts.host]
        .into_iter()
        .chain(parts.path.as_ref())
        .any(|v| v.contains(&b'\n'))
    {
        return None;
    }
    Some(Credential {
        protocol: Some(line[..sep].into()),
        host: Some(parts.host),
        path: parts.path,
        username: Some(username),
        password: Some(password),
        ..Credential::default()
    })
}

/// The lines of a credential file's content.
pub fn lines(content: &[u8]) -> impl Iterator<Item = &[u8]> {
    let content = content.strip_suffix(b"\n").unwrap_or(content);
    content
        .split(|&b| b == b'\n')
        .filter(move |_| !content.is_empty())
}

/// The first credential in `content` that `want` matches.
pub fn find(content: &[u8], want: &Credential) -> Option<Credential> {
    lines(content)
        .filter_map(parse_line)
        .find(|have| want.matches(have, false))
}

/// The new content of a file: `first` (if any) on top, then every line of
/// `content` except the credentials `want` matches.
pub fn rewrite(
    content: &[u8],
    first: Option<&[u8]>,
    want: &Credential,
    match_password: bool,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 64);
    if let Some(line) = first {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    for line in lines(content) {
        if parse_line(line).is_some_and(|have| want.matches(&have, match_password)) {
            continue;
        }
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    out
}

/// What `get` prints for a found credential.
pub fn answer(have: &Credential) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in [("username", &have.username), ("password", &have.password)] {
        if let Some(v) = v {
            out.extend_from_slice(k.as_bytes());
            out.push(b'=');
            out.extend_from_slice(v);
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(lines: &str) -> Credential {
        super::super::read_helper_request(&mut lines.as_bytes()).unwrap()
    }

    #[test]
    fn encodes_like_git() {
        let c = cred(
            "protocol=https\nhost=ex.com:8080\npath=a b/c%d!:@'\nusername=u s@:/x\npassword=p:@/ %\u{e4}~-._\n",
        );
        assert_eq!(
            format_line(&c).unwrap(),
            "https://u%20s%40%3a%2fx:p%3a%40%2f%20%25%c3%a4~-._@ex.com%3a8080/a%20b/c%25d!:@'"
        );
        let c = cred("protocol=https\nhost=sso.tld\nusername=\npassword=\n");
        assert_eq!(format_line(&c).unwrap(), "https://:@sso.tld");
        assert!(format_line(&cred("protocol=https\nhost=h\nusername=u\n")).is_none());
    }

    #[test]
    fn round_trip() {
        let c = cred("protocol=https\nhost=h:1\npath=a b/c\nusername=u:@\npassword=p%\n");
        let back = parse_line(&format_line(&c).unwrap()).unwrap();
        assert_eq!(back.protocol, c.protocol);
        assert_eq!(back.host, c.host);
        assert_eq!(back.path, c.path);
        assert_eq!(back.username, c.username);
        assert_eq!(back.password, c.password);
    }

    #[test]
    fn invalid_lines() {
        for l in [
            "",
            "#comment",
            "\t",
            "://user:pass@example.com",
            "https://user:pass@",
            "https://pass@example.com",
        ] {
            assert!(parse_line(l.as_bytes()).is_none(), "{l:?}");
        }
        assert!(parse_line(b"file://u:p@/some/path").is_some());
    }

    #[test]
    fn lookup_and_rewrite() {
        let content = b"https://a:b@Ex.com\nhttps://c:d@ex.com/org\nbogus\nhttps://e:f@ex.com\n";
        let want = cred("protocol=https\nhost=ex.com\n");
        assert_eq!(find(content, &want).unwrap().username.unwrap(), "c");
        let want = cred("protocol=https\nhost=ex.com\npath=other\n");
        assert!(find(content, &want).is_none());
        let want = cred("protocol=https\nhost=ex.com\nusername=e\npassword=zz\n");
        assert_eq!(rewrite(content, None, &want, true), content);
        assert_eq!(
            rewrite(content, Some(b"https://n:m@ex.com"), &want, false),
            b"https://n:m@ex.com\nhttps://a:b@Ex.com\nhttps://c:d@ex.com/org\nbogus\n"
        );
    }
}
