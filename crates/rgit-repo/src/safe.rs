// SPDX-License-Identifier: MIT
//! Ownership checks with `safe.directory`, and `safe.bareRepository`
//! (`Documentation/config/safe.adoc`). Both settings are only read from
//! protected configuration (system, global, command line).

use std::path::Path;

use bstr::ByteSlice;
use rgit_config::{ConfigSet, Environment, Origin};

use crate::error::Error;
use crate::messages;
use crate::path;

/// Who may own a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner {
    /// The effective user id.
    pub uid: u32,
    /// `SUDO_UID` when running as root.
    pub sudo_uid: Option<u32>,
    /// `GIT_TEST_ASSUME_DIFFERENT_OWNER`: treat every path as foreign.
    pub assume_different: bool,
}

impl Owner {
    /// The current process, with `SUDO_UID` and
    /// `GIT_TEST_ASSUME_DIFFERENT_OWNER` from `env`.
    pub fn current(env: &dyn Environment) -> Owner {
        let uid = effective_uid();
        let sudo_uid = if uid == 0 {
            env.var("SUDO_UID")
                .and_then(|v| v.to_str().and_then(parse_sudo_uid))
        } else {
            None
        };
        Owner {
            uid,
            sudo_uid,
            assume_different: env_bool(env, "GIT_TEST_ASSUME_DIFFERENT_OWNER"),
        }
    }

    /// Whether `p` belongs to this user.
    pub fn owns(&self, p: &Path) -> bool {
        if self.assume_different {
            return false;
        }
        match owner_of(p) {
            Some(u) => u == self.uid || Some(u) == self.sudo_uid,
            None => false,
        }
    }
}

/// `SUDO_UID` as git reads it (observed with git 2.56): a decimal number
/// with leading white space and a sign allowed, nothing after it, at most
/// 64 bits (a negative number wraps around); the user id is its low 32
/// bits.
fn parse_sudo_uid(s: &str) -> Option<u32> {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value: u64 = digits.parse().ok()?;
    let value = if negative {
        value.wrapping_neg()
    } else {
        value
    };
    Some(value as u32)
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(not(unix))]
fn effective_uid() -> u32 {
    0
}

#[cfg(unix)]
fn owner_of(p: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn owner_of(_p: &Path) -> Option<u32> {
    Some(0)
}

pub(crate) fn env_bool(env: &dyn Environment, name: &str) -> bool {
    env.var_bytes(name)
        .and_then(|v| rgit_config::value::parse_bool(Some(&v)))
        .unwrap_or(false)
}

/// Whether `checked` (an absolute, resolved path) is listed in
/// `safe.directory`. `.` stands for `cwd`; other relative entries are
/// skipped with a warning (returned without `warning: `).
pub fn is_safe_directory(
    protected: &ConfigSet,
    env: &dyn Environment,
    checked: &Path,
    cwd: &Path,
) -> (bool, Vec<String>) {
    let mut safe = false;
    let mut warnings = Vec::new();
    for e in protected.get_all("safe.directory") {
        let Some(raw) = e.value.as_ref() else {
            continue;
        };
        if raw.is_empty() {
            safe = false;
            continue;
        }
        if raw.as_slice() == b"*" {
            safe = true;
            continue;
        }
        let value = if raw.as_slice() == b"." {
            cwd.to_path_buf()
        } else {
            match e.path(env) {
                Ok(Some(v)) => v,
                _ => continue,
            }
        };
        if !value.is_absolute() {
            warnings.push(messages::safe_directory_not_absolute(raw.as_bstr()));
            continue;
        }
        if matches_entry(&value, checked) {
            safe = true;
        }
    }
    (safe, warnings)
}

fn matches_entry(value: &Path, checked: &Path) -> bool {
    let root = Path::new("/");
    let bytes = value.as_os_str().as_encoded_bytes();
    if let Some(base) = bytes.strip_suffix(b"/*") {
        let base = crate::layout::bytes_to_path(base);
        let dir = path::real_path(&base, root).unwrap_or(base);
        return path::strictly_inside(checked, &dir).is_some();
    }
    match path::real_path(value, root) {
        Ok(r) => r == checked,
        Err(_) => value == checked,
    }
}

/// `safe.bareRepository`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BareRepository {
    /// All bare repositories (the default).
    #[default]
    All,
    /// Only bare repositories named with `GIT_DIR` / `--git-dir`.
    Explicit,
}

/// The effective `safe.bareRepository`.
pub fn bare_repository_setting(protected: &ConfigSet) -> Result<BareRepository, Error> {
    let Some(e) = protected.get("safe.barerepository") else {
        return Ok(BareRepository::All);
    };
    match e.value.as_ref().map(|v| v.as_bytes()) {
        Some(b"all") => Ok(BareRepository::All),
        Some(b"explicit") => Ok(BareRepository::Explicit),
        _ => {
            let source = match &e.source.origin {
                Origin::CommandLine => "command-line config".to_string(),
                o => o.describe(),
            };
            Err(Error::BadProtectedValue(messages::unable_to_parse(
                "safe.barerepository",
                source,
            )))
        }
    }
}

/// A bare repository found by discovery that is part of a non-bare
/// repository (its `.git` directory, a linked work tree's or a submodule's
/// git directory) is not subject to `safe.bareRepository=explicit`.
pub(crate) fn is_embedded_git_dir(dir: &Path) -> bool {
    dir.components().any(|c| c.as_os_str() == ".git") || dir.join("commondir").is_file()
}

/// The ownership check of a server entering a repository it was asked
/// for (`receive-pack`, `upload-archive`, `http-backend`, `daemon`;
/// `upload-pack` does not check). `found` is where the request led (a
/// git directory or a `.git` file naming one), `git_dir` the git
/// directory. Both must belong to `owner`, or `found` (with links
/// resolved) must be listed in `safe.directory`, with `.` standing for
/// `cwd`. The error names `git_dir` as given (observed with git 2.56).
/// Returns the warnings to print (without `warning: `).
pub fn check_server_ownership(
    owner: &Owner,
    protected: &mut dyn crate::ProtectedConfig,
    found: &Path,
    git_dir: &Path,
    cwd: &Path,
) -> Result<Vec<String>, Error> {
    if owner.owns(found) && owner.owns(git_dir) {
        return Ok(Vec::new());
    }
    let checked = path::real_path_lossy(found, cwd);
    let set = protected.load()?;
    let (safe, warnings) = is_safe_directory(&set, protected.env(), &checked, cwd);
    if safe {
        Ok(warnings)
    } else {
        Err(Error::DubiousOwnership {
            path: git_dir.to_path_buf(),
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::parse_sudo_uid;

    #[test]
    fn sudo_uid() {
        assert_eq!(parse_sudo_uid("1000"), Some(1000));
        assert_eq!(parse_sudo_uid(" \t+1000"), Some(1000));
        assert_eq!(parse_sudo_uid("01000"), Some(1000));
        assert_eq!(parse_sudo_uid("4294968296"), Some(1000));
        assert_eq!(parse_sudo_uid("-4294966296"), Some(1000));
        assert_eq!(parse_sudo_uid("-1"), Some(u32::MAX));
        for bad in ["", "-", "1000 ", "0x3e8", "1000.0", "18446744073709552616"] {
            assert_eq!(parse_sudo_uid(bad), None, "{bad:?}");
        }
    }
}
