// SPDX-License-Identifier: MIT
//! Repository URLs: classification (`<transport>::<address>`,
//! `<scheme>://`, scp-like `[user@]host:path`, local paths), the split git's
//! native transports use (protocol, user and host, port, path), and the
//! "humanish" directory name of `git clone`.
//!
//! Syntax from `gitglossary`/`git-clone(1)` "GIT URLS"; the exact splitting
//! rules were inferred from `git fetch-pack --diag-url` and `git clone`
//! output.

use bstr::{BString, ByteSlice};

use crate::messages;

/// Whether `c` may appear in a URL scheme or a remote helper name.
fn is_scheme_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.')
}

/// The `<transport>` of `<transport>::<address>` and the address. The
/// transport may be empty.
pub fn helper_prefix(url: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = url.iter().take_while(|&&c| is_scheme_char(c)).count();
    url[n..]
        .starts_with(b"::")
        .then(|| (&url[..n], &url[n + 2..]))
}

/// The scheme of `<scheme>://...` when the scheme is non-empty and made of
/// letters, digits, `+`, `-` and `.`.
pub fn url_scheme(url: &[u8]) -> Option<&[u8]> {
    let n = url.iter().take_while(|&&c| is_scheme_char(c)).count();
    (n > 0 && url[n..].starts_with(b"://")).then(|| &url[..n])
}

/// Whether `url` looks like a URL (`<scheme>://`).
pub fn is_url(url: &[u8]) -> bool {
    url_scheme(url).is_some()
}

/// Whether `url` is a local path rather than scp-like `host:path`: it has
/// no `:`, or a `/` comes before the first `:`.
pub fn is_local_path(url: &[u8]) -> bool {
    match url.find_byte(b':') {
        None => true,
        Some(colon) => url[..colon].contains(&b'/'),
    }
}

/// How git reaches a repository given by URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A remote helper `git-remote-<name>`: `<name>::<address>` or a URL
    /// whose scheme git does not handle itself (`http`, `https`, `ftp` and
    /// `ftps` are helpers too).
    Helper {
        /// The helper name (also the protocol name for `protocol.*.allow`).
        name: BString,
        /// What the helper is given: the address after `::`, or the whole
        /// URL.
        address: BString,
    },
    /// git's own transports: `git://`, `ssh://`, scp-like, `file://` and
    /// local paths (a bundle file is detected by the caller). Use
    /// [`parse_connect_url`] for the details.
    Native,
}

impl Transport {
    /// Whether this is one of the curl-based helpers.
    pub fn is_curl(&self) -> bool {
        matches!(self, Transport::Helper { name, address } if is_url(address) && matches!(name.as_slice(), b"http" | b"https" | b"ftp" | b"ftps"))
    }
}

/// Classify a URL (after `insteadOf` rewriting).
pub fn classify(url: &[u8]) -> Transport {
    if let Some((name, address)) = helper_prefix(url) {
        return Transport::Helper {
            name: name.into(),
            address: address.into(),
        };
    }
    match url_scheme(url) {
        Some(b"git" | b"ssh" | b"git+ssh" | b"ssh+git" | b"file") | None => Transport::Native,
        Some(scheme) => Transport::Helper {
            name: scheme.into(),
            address: url.into(),
        },
    }
}

/// The protocols of git's native transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Local path or `file://`.
    File,
    /// `git://`.
    Git,
    /// `ssh://`, `git+ssh://`, `ssh+git://` or scp-like.
    Ssh,
}

impl Protocol {
    /// The name used in messages and `protocol.<name>.allow`.
    pub fn name(self) -> &'static str {
        match self {
            Protocol::File => "file",
            Protocol::Git => "git",
            Protocol::Ssh => "ssh",
        }
    }
}

/// A URL split for a native transport, as `git fetch-pack --diag-url`
/// shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectUrl {
    /// The protocol.
    pub protocol: Protocol,
    /// SSH: `[user@]host` without brackets and port (`userandhost`).
    /// git and file: the raw authority (`hostandport`, empty for local
    /// paths).
    pub host: BString,
    /// SSH only: the port as written, when one was given.
    pub port: Option<BString>,
    /// The path; for SSH and git, a leading `/~` becomes `~`.
    pub path: BString,
}

/// Why a URL cannot be used with a native transport. `Display` is git's
/// message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectUrlError {
    /// `<scheme>://` with a scheme git cannot connect to itself.
    #[error("{}", messages::protocol_not_supported(.0.as_bstr()))]
    UnsupportedProtocol(BString),
    /// No path after the host.
    #[error("{}", messages::NO_PATH)]
    NoPath,
}

/// Split `url` the way git's native transports do.
pub fn parse_connect_url(url: &[u8]) -> Result<ConnectUrl, ConnectUrlError> {
    let (protocol, host, path) = if let Some(i) = url.find(b"://") {
        let protocol = match &url[..i] {
            b"ssh" | b"git+ssh" | b"ssh+git" => Protocol::Ssh,
            b"git" => Protocol::Git,
            b"file" => Protocol::File,
            other => return Err(ConnectUrlError::UnsupportedProtocol(other.into())),
        };
        let rest = percent_decode(&url[i + 3..]);
        let start = host_end(&rest);
        let slash = rest[start..]
            .find_byte(b'/')
            .ok_or(ConnectUrlError::NoPath)?
            + start;
        (
            protocol,
            BString::from(&rest[..slash]),
            BString::from(&rest[slash..]),
        )
    } else if is_local_path(url) {
        return Ok(ConnectUrl {
            protocol: Protocol::File,
            host: BString::default(),
            port: None,
            path: url.into(),
        });
    } else {
        let start = host_end(url);
        let colon = url[start..]
            .find_byte(b':')
            .ok_or(ConnectUrlError::NoPath)?
            + start;
        (
            Protocol::Ssh,
            BString::from(&url[..colon]),
            BString::from(&url[colon + 1..]),
        )
    };
    // `/~user/x` and `host:/~user/x` name a home directory. git drops the
    // first character whenever the second is `~` (so `host:a~` is `~`).
    let path = if protocol != Protocol::File && path.get(1) == Some(&b'~') {
        BString::from(&path[1..])
    } else {
        path
    };
    let (host, port) = if protocol == Protocol::Ssh {
        split_ssh_host(&host)
    } else {
        (host, None)
    };
    Ok(ConnectUrl {
        protocol,
        host,
        port,
        path,
    })
}

/// Where a bracketed host (`[...]`, also after `user@`) ends, so that the
/// separator after the host is searched only from there.
fn host_end(s: &[u8]) -> usize {
    match bracket(s) {
        Some((_, close)) => close + 1,
        None => 0,
    }
}

/// The positions of `[` and `]` of a bracketed host: one at the start, or
/// one right after an `@`.
fn bracket(s: &[u8]) -> Option<(usize, usize)> {
    let open = if s.first() == Some(&b'[') {
        0
    } else {
        s.find(b"@[")? + 1
    };
    let close = s[open..].find_byte(b']')? + open;
    Some((open, close))
}

/// Split the user-and-host part of an SSH URL into host and port.
fn split_ssh_host(raw: &[u8]) -> (BString, Option<BString>) {
    let (host, port) = host_and_port(raw);
    if port.is_some() {
        return (host, port);
    }
    // A port inside the brackets (`[host:22]`).
    host_and_port(&host)
}

fn host_and_port(s: &[u8]) -> (BString, Option<BString>) {
    let (host, tail_start) = match bracket(s) {
        Some((open, close)) => {
            let mut h = BString::from(&s[..open]);
            h.extend_from_slice(&s[open + 1..close]);
            (h, Some(close + 1))
        }
        None => (BString::from(s), None),
    };
    let search_from = tail_start.unwrap_or(0);
    let Some(colon) = s[search_from..].find_byte(b':').map(|c| c + search_from) else {
        return (host, None);
    };
    let candidate = &s[colon + 1..];
    let plain_host = |h: BString| match tail_start {
        Some(_) => h,
        None => BString::from(&s[..colon]),
    };
    if candidate.is_empty() {
        (plain_host(host), None)
    } else if is_valid_port(candidate) {
        (plain_host(host), Some(candidate.into()))
    } else {
        (host, None)
    }
}

/// A decimal port number in 0..=65535, with optional leading whitespace
/// and sign, and nothing after the digits.
fn is_valid_port(s: &[u8]) -> bool {
    let s = s.trim_start_with(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    let (neg, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let mut value: u64 = 0;
    for &d in digits {
        value = value.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    if neg { value == 0 } else { value <= 65535 }
}

/// Decode `%XX` escapes; invalid escapes and `%00` stay as written.
pub fn percent_decode(s: &[u8]) -> BString {
    let mut out = BString::default();
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2])) {
                let b = (h * 16 + l) as u8;
                if b != 0 {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// The error of [`humanish_dir`]; `Display` is git's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", messages::NO_DIRECTORY_NAME)]
pub struct NoDirectoryName;

/// The directory `git clone <url>` creates when none is given: the last
/// path component without `/.git` and `.git` (and `.bundle` for a bundle
/// file), or the host when the URL has no path; `.git` is appended for a
/// bare clone. Control characters and runs of whitespace become one
/// space, and leading and trailing whitespace is removed.
pub fn humanish_dir(url: &[u8], bare: bool, is_bundle: bool) -> Result<BString, NoDirectoryName> {
    let mut start = url_scheme(url).map_or(0, |s| s.len() + 3);
    let mut end = url.len();
    let trim_slashes = |end: &mut usize| {
        while *end > start && url[*end - 1] == b'/' {
            *end -= 1;
        }
    };
    trim_slashes(&mut end);
    if url[start..end].ends_with(b"/.git") {
        end -= 5;
        trim_slashes(&mut end);
    }
    if !url[start..end].contains(&b'/') {
        // Only an authority: drop user information and a numeric port.
        if let Some(at) = url[start..end].rfind_byte(b'@') {
            start += at + 1;
        }
        if let Some(colon) = url[start..end].rfind_byte(b':') {
            if url[start + colon + 1..end].iter().all(u8::is_ascii_digit) {
                end = start + colon;
            }
        }
    }
    if let Some(sep) = url[start..end]
        .iter()
        .rposition(|&c| c == b'/' || c == b':')
    {
        start += sep + 1;
    }
    let mut name = &url[start..end];
    if is_bundle && name.ends_with(b".bundle") {
        name = &name[..name.len() - 7];
    } else if name.ends_with(b".git") {
        name = &name[..name.len() - 4];
    }
    if name.is_empty() || (name.len() == 1 && name[0] == b'/') {
        return Err(NoDirectoryName);
    }
    let mut dir = BString::from(name);
    if bare {
        dir.extend_from_slice(b".git");
    }
    let mut out = BString::default();
    let mut pending_space = false;
    for &c in dir.iter() {
        if c < 0x20 || c == 0x7f || c.is_ascii_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(b' ');
        }
        pending_space = false;
        out.push(c);
    }
    if out.is_empty() {
        return Err(NoDirectoryName);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(url: &str) -> String {
        match parse_connect_url(url.as_bytes()) {
            Ok(c) => match c.protocol {
                Protocol::Ssh => format!(
                    "ssh|{}|{}|{}",
                    c.host,
                    c.port.map_or("NONE".to_string(), |p| p.to_string()),
                    c.path
                ),
                p => format!("{}|{}|{}", p.name(), c.host, c.path),
            },
            Err(e) => format!("fatal: {e}"),
        }
    }

    #[test]
    fn connect_urls() {
        let cases = [
            ("host:path", "ssh|host|NONE|path"),
            ("user@host:~user/p", "ssh|user@host|NONE|~user/p"),
            ("ssh://u@[::1]:22/~/x", "ssh|u@::1|22|~/x"),
            ("[host:22]:p", "ssh|host|22|p"),
            ("git://h:9418/~u/x", "git|h:9418|~u/x"),
            ("file:///tmp/x", "file||/tmp/x"),
            ("./foo:bar", "file||./foo:bar"),
            ("ssh://h:/x", "ssh|h|NONE|/x"),
            ("h:", "ssh|h|NONE|"),
            ("[::1]:x", "ssh|::1|NONE|x"),
            ("ssh://%41@h/%41", "ssh|A@h|NONE|/A"),
            (
                "ssh://h",
                "fatal: no path specified; see 'git help pull' for valid url syntax",
            ),
            ("x::y", "ssh|x|NONE|:y"),
            ("ssh://h:22x/p", "ssh|h:22x|NONE|/p"),
            ("ssh://h:+22/x", "ssh|h|+22|/x"),
            ("ssh://h:-1/x", "ssh|h:-1|NONE|/x"),
            ("ssh://h: 22/x", "ssh|h| 22|/x"),
            ("ssh://h:65536/x", "ssh|h:65536|NONE|/x"),
            ("ssh://h:-0/x", "ssh|h|-0|/x"),
            ("ssh://[h:22]:33/x", "ssh|h:22|33|/x"),
            ("[h:22]:33:x", "ssh|h|22|33:x"),
            ("ssh://a@b@[h]:5/x", "ssh|a@b@h|5|/x"),
            ("x@[h]y:x", "ssh|x@h|NONE|x"),
            ("[a@b]@c:x", "ssh|a@b|NONE|x"),
            ("ssh://[::1]x:22/y", "ssh|::1|22|/y"),
            ("ssh://[::1/x", "ssh|[::1|NONE|/x"),
            ("[::1:x", "ssh|[|NONE|:1:x"),
            ("ssh://u@::1:22/x", "ssh|u@::1:22|NONE|/x"),
            ("ssh://[::1]:22:x/y", "ssh|::1|NONE|/y"),
            ("ssh://h/%2F~x", "ssh|h|NONE|//~x"),
            ("ssh://h%2Fx/y", "ssh|h|NONE|/x/y"),
            ("ssh://h/a%00b", "ssh|h|NONE|/a%00b"),
            ("ssh://h/%4", "ssh|h|NONE|/%4"),
            (
                "git://h",
                "fatal: no path specified; see 'git help pull' for valid url syntax",
            ),
            ("git://[::1]:1/x", "git|[::1]:1|/x"),
            ("file://host/tmp/x", "file|host|/tmp/x"),
            ("file:/x", "ssh|file|NONE|/x"),
            ("FILE:///x", "fatal: protocol 'FILE' is not supported"),
            ("a/b://c", "fatal: protocol 'a/b' is not supported"),
            ("://x", "fatal: protocol '' is not supported"),
            (
                "[::1]",
                "fatal: no path specified; see 'git help pull' for valid url syntax",
            ),
            ("h:22:x", "ssh|h|NONE|22:x"),
            ("ssh:///x", "ssh||NONE|/x"),
            ("h:a~", "ssh|h|NONE|~"),
            ("h::~x", "ssh|h|NONE|~x"),
            ("h::/~x", "ssh|h|NONE|:/~x"),
            ("ssh://h//~", "ssh|h|NONE|//~"),
        ];
        for (url, want) in cases {
            assert_eq!(diag(url), want, "{url}");
        }
    }

    #[test]
    fn classification() {
        let helper = |u: &str| match classify(u.as_bytes()) {
            Transport::Helper { name, .. } => Some(name.to_string()),
            Transport::Native => None,
        };
        assert_eq!(helper("foo::bar").as_deref(), Some("foo"));
        assert_eq!(helper("::c").as_deref(), Some(""));
        assert_eq!(helper("a.b::c").as_deref(), Some("a.b"));
        assert_eq!(helper("a_b::c"), None);
        assert_eq!(helper("a/b::c"), None);
        assert_eq!(helper("https://h/x").as_deref(), Some("https"));
        assert_eq!(helper("FOO://h/x").as_deref(), Some("FOO"));
        assert_eq!(helper("1a://h/x").as_deref(), Some("1a"));
        assert_eq!(helper("foo::bar://x").as_deref(), Some("foo"));
        assert_eq!(helper("a_b://h/x"), None);
        assert_eq!(helper("git+ssh://h/x"), None);
        assert_eq!(helper("://x"), None);
        assert!(classify(b"https://h/x").is_curl());
        assert!(!classify(b"zz::https://h/x").is_curl());
    }

    #[test]
    fn humanish() {
        let h = |u: &str, bare: bool| {
            humanish_dir(u.as_bytes(), bare, false)
                .map(|b| b.to_string())
                .unwrap_or_else(|e| e.to_string())
        };
        let cases = [
            ("host:foo", "foo"),
            ("host:foo.git", "foo"),
            ("host:foo/.git", "foo"),
            ("ssh://host/foo///.git/", "foo"),
            ("ssh://host/foo.git///", "foo"),
            ("ssh://host/", "host"),
            ("ssh://host:1234/", "host"),
            ("ssh://user:passw@rd@host:1234/", "host"),
            ("user:passw@rd@host:/", "host"),
            ("host:/", "host"),
            ("ssh://host/foo@bar.git", "foo@bar"),
            ("ssh://user:password@host/test:1234.git", "1234"),
            ("host:", "host"),
            ("ssh://host/.git", "host"),
            ("ssh://host/foo.git.git", "foo.git"),
            ("ssh://host/a\t\tb", "a b"),
            ("ssh://host/ a", "a"),
            ("ssh://[::1]/", "1]"),
            ("ssh://h@[::1]:22/", "1]"),
            ("ssh://host:12a/", "12a"),
            (
                "ssh://host/x/.git/.git",
                "No directory name could be guessed.\nPlease specify a directory on the command line",
            ),
            (
                "zz::/",
                "No directory name could be guessed.\nPlease specify a directory on the command line",
            ),
            ("zz::host/foo/bar.git/", "bar"),
            ("host:foo@bar", "bar"),
            ("ssh://host/x.GIT", "x.GIT"),
            ("ssh://host/~user", "~user"),
        ];
        for (url, want) in cases {
            assert_eq!(h(url, false), want, "{url}");
        }
        assert_eq!(h("host:foo", true), "foo.git");
        assert_eq!(h("ssh://host/a .git", false), "a");
        assert_eq!(h("ssh://host/a .git", true), "a .git");
        assert_eq!(humanish_dir(b"/tmp/x.bundle", false, true).unwrap(), "x");
    }
}
