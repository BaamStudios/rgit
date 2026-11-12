// SPDX-License-Identifier: MIT
//! The `agent=` capability value.
//!
//! Observed from git 2.56.0 (and the expectations in t5701): the value is
//! `git/<version>-<sysname>`, where `<sysname>` is the `uname -s` field
//! (`Linux`), in protocol v0, v1 and v2 alike. `GIT_USER_AGENT` replaces the
//! whole value. Every byte outside `0x21..=0x7e` is replaced by `.` (so
//! `GIT_USER_AGENT="a bé"` is sent as `a.b..`).
//!
//! rgit reports its version as `2.56.1.rgit.<crate version>` (CLEANROOM.md,
//! "Version string"), so the default agent is
//! `git/2.56.1.rgit.0.1.0-Linux`.

use std::ffi::OsStr;
use std::sync::OnceLock;

/// The git version rgit is compatible with.
pub const GIT_COMPAT_VERSION: &str = "2.56.1";

/// `2.56.1.rgit.<crate version>`, the third field of `rgit version`.
pub fn version() -> String {
    format!("{GIT_COMPAT_VERSION}.rgit.{}", env!("CARGO_PKG_VERSION"))
}

/// Replace every byte outside printable non-space ASCII by `.`.
pub fn redact(raw: &[u8]) -> String {
    raw.iter()
        .map(|&b| {
            if (0x21..=0x7e).contains(&b) {
                b as char
            } else {
                '.'
            }
        })
        .collect()
}

/// The operating system name as `uname -s` prints it.
pub fn sysname() -> String {
    #[cfg(unix)]
    {
        let uts = rustix::system::uname();
        String::from_utf8_lossy(uts.sysname().to_bytes()).into_owned()
    }
    #[cfg(not(unix))]
    {
        match std::env::consts::OS {
            "windows" => "Windows".to_owned(),
            other => other.to_owned(),
        }
    }
}

/// The agent string for a given `GIT_USER_AGENT` value and system name.
pub fn agent_from(user_agent: Option<&OsStr>, sysname: &str) -> String {
    match user_agent {
        Some(ua) => redact(ua.as_encoded_bytes()),
        None => redact(format!("git/{}-{sysname}", version()).as_bytes()),
    }
}

/// The agent string of this process (`GIT_USER_AGENT` or the default),
/// computed once.
pub fn agent() -> &'static str {
    static AGENT: OnceLock<String> = OnceLock::new();
    AGENT.get_or_init(|| agent_from(std::env::var_os("GIT_USER_AGENT").as_deref(), &sysname()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_agent() {
        assert_eq!(
            agent_from(None, "Linux"),
            format!("git/2.56.1.rgit.{}-Linux", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(agent_from(None, "My OS"), agent_from(None, "My.OS"));
    }

    #[test]
    fn user_agent_is_redacted() {
        assert_eq!(agent_from(Some(OsStr::new("FAKE")), "Linux"), "FAKE");
        assert_eq!(agent_from(Some(OsStr::new("a bé")), "Linux"), "a.b..");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysname_linux() {
        assert_eq!(sysname(), "Linux");
    }

    #[test]
    fn process_agent_is_printable() {
        assert!(agent().bytes().all(|b| (0x21..=0x7e).contains(&b)));
    }
}
