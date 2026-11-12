// SPDX-License-Identifier: MIT
//! What the system says about the current user, for guessing an identity.
//!
//! The guessed name is the full name from the password database (the
//! GECOS field up to the first `,`, with `&` standing for the capitalised
//! login name). The guessed email is `<login>@<mail domain>`, where the
//! domain is the first line of `/etc/mailname`, else the host name if it
//! contains a dot, else the fully qualified name found for the host name
//! in `/etc/hosts`; failing all that it is `<host name>.(none)` and the
//! address counts as bogus.
//!
//! rgit reads `/etc/passwd` and `/etc/hosts` directly instead of calling
//! `getpwuid` and `getaddrinfo` (no `unsafe` FFI, see ADR 0003), so users
//! and host names that only NSS (LDAP, DNS) knows are not found.

use bstr::{BString, ByteSlice};

/// Everything needed to guess an identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserInfo {
    /// The login name; `None` if the user is not in the password database.
    pub login: Option<BString>,
    /// The full name derived from the GECOS field (may be empty).
    pub full_name: BString,
    /// The mail domain.
    pub domain: String,
    /// The domain had to be made up (`<host>.(none)`).
    pub domain_bogus: bool,
}

impl UserInfo {
    /// Look up the current user and host.
    pub fn from_system() -> Self {
        let passwd = std::fs::read("/etc/passwd").unwrap_or_default();
        let (login, full_name) = match passwd_entry(&passwd, current_uid()) {
            Some((login, gecos)) => {
                let full = full_name_from_gecos(&gecos, &login);
                (Some(login), full)
            }
            None => (None, BString::default()),
        };
        let mailname = std::fs::read("/etc/mailname").ok();
        let hosts = std::fs::read("/etc/hosts").unwrap_or_default();
        let (domain, domain_bogus) = mail_domain(mailname.as_deref(), &hostname(), &hosts);
        UserInfo {
            login,
            full_name,
            domain,
            domain_bogus,
        }
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    u32::MAX
}

/// The host name (`uname -n`).
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let uts = rustix::system::uname();
        String::from_utf8_lossy(uts.nodename().to_bytes()).into_owned()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "localhost".into())
    }
}

/// The (login, GECOS) of `uid` in `/etc/passwd` content.
pub fn passwd_entry(passwd: &[u8], uid: u32) -> Option<(BString, BString)> {
    let uid = uid.to_string();
    for line in passwd.lines() {
        let fields: Vec<&[u8]> = line.split(|&b| b == b':').collect();
        if fields.len() >= 5 && fields[2] == uid.as_bytes() {
            return Some((fields[0].into(), fields[4].into()));
        }
    }
    None
}

/// The full name in a GECOS field: up to the first `,`, `&` replaced by
/// the login name with its first letter in upper case.
pub fn full_name_from_gecos(gecos: &[u8], login: &[u8]) -> BString {
    let field = gecos.split(|&b| b == b',').next().unwrap_or_default();
    let mut out = BString::default();
    for &b in field {
        if b == b'&' {
            if let Some((first, rest)) = login.split_first() {
                out.push(first.to_ascii_uppercase());
                out.extend_from_slice(rest);
            }
        } else {
            out.push(b);
        }
    }
    out
}

/// The mail domain and whether it is made up, from `/etc/mailname`
/// content, the host name and `/etc/hosts` content.
pub fn mail_domain(mailname: Option<&[u8]>, host: &str, hosts: &[u8]) -> (String, bool) {
    if let Some(m) = mailname {
        let first = m.lines().next().unwrap_or_default().trim_ascii();
        if !first.is_empty() {
            return (first.to_str_lossy().into_owned(), false);
        }
    }
    if host.contains('.') {
        return (host.to_owned(), false);
    }
    if let Some(fqdn) = canonical_from_hosts(hosts, host) {
        if fqdn.contains('.') {
            return (fqdn, false);
        }
    }
    (format!("{host}.(none)"), true)
}

/// The canonical (first) name of the `/etc/hosts` line that lists `host`.
fn canonical_from_hosts(hosts: &[u8], host: &str) -> Option<String> {
    for line in hosts.lines() {
        let line = match line.find_byte(b'#') {
            Some(i) => &line[..i],
            None => line,
        };
        let mut names = line.fields_with(|c| c.is_ascii_whitespace()).skip(1);
        let Some(canonical) = names.next() else {
            continue;
        };
        let matches = canonical.eq_ignore_ascii_case(host.as_bytes())
            || names.any(|n| n.eq_ignore_ascii_case(host.as_bytes()));
        if matches {
            return Some(canonical.to_str_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gecos() {
        assert_eq!(full_name_from_gecos(b"Pat Doe,Room 1,,", b"pat"), "Pat Doe");
        assert_eq!(full_name_from_gecos(b"& Doe", b"pat"), "Pat Doe");
        assert_eq!(full_name_from_gecos(b"", b"pat"), "");
    }

    #[test]
    fn passwd() {
        let data = b"root:x:0:0:root:/root:/bin/bash\nu:x:1000:1000:U Ser,,:/home/u:/bin/sh\n";
        assert_eq!(
            passwd_entry(data, 1000),
            Some(("u".into(), "U Ser,,".into()))
        );
        assert_eq!(passwd_entry(data, 7), None);
    }

    #[test]
    fn domain() {
        assert_eq!(
            mail_domain(Some(b"mail.example.org\n"), "desk", b""),
            ("mail.example.org".into(), false)
        );
        assert_eq!(
            mail_domain(None, "desk.example.org", b""),
            ("desk.example.org".into(), false)
        );
        let hosts = b"127.0.0.1 localhost\n10.0.0.2 desk.lan desk # me\n";
        assert_eq!(mail_domain(None, "desk", hosts), ("desk.lan".into(), false));
        assert_eq!(mail_domain(None, "box", hosts), ("box.(none)".into(), true));
        assert_eq!(
            mail_domain(Some(b"\n"), "box", b""),
            ("box.(none)".into(), true)
        );
    }
}
