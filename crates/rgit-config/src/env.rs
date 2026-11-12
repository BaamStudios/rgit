// SPDX-License-Identifier: MIT
//! Access to the process environment, abstracted for tests.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use bstr::BString;

use crate::origin::path_bytes;

/// What configuration loading needs from the environment.
pub trait Environment {
    /// An environment variable.
    fn var(&self, name: &str) -> Option<OsString>;

    /// The home directory of `user` (for `~user/` expansion).
    fn user_home(&self, user: &str) -> Option<PathBuf>;

    /// git's runtime prefix (for `%(prefix)/` expansion).
    fn runtime_prefix(&self) -> Option<PathBuf> {
        None
    }

    /// An environment variable as bytes.
    fn var_bytes(&self, name: &str) -> Option<BString> {
        self.var(name).map(|v| path_bytes(std::path::Path::new(&v)))
    }
}

/// The real process environment.
#[derive(Debug, Clone, Default)]
pub struct SystemEnv {
    /// Runtime prefix (`/usr` for a system installation).
    pub prefix: Option<PathBuf>,
}

impl Environment for SystemEnv {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn user_home(&self, user: &str) -> Option<PathBuf> {
        passwd_home(&std::fs::read("/etc/passwd").ok()?, user)
    }

    fn runtime_prefix(&self) -> Option<PathBuf> {
        self.prefix.clone()
    }
}

/// Look up a user's home directory in `/etc/passwd` content.
///
/// This does not consult NSS (LDAP and similar); rgit avoids `unsafe` FFI to
/// `getpwnam`, see ADR 0003.
fn passwd_home(passwd: &[u8], user: &str) -> Option<PathBuf> {
    for line in passwd.split(|&b| b == b'\n') {
        let fields: Vec<&[u8]> = line.split(|&b| b == b':').collect();
        if fields.len() >= 6 && fields[0] == user.as_bytes() {
            return Some(crate::value::bytes_to_path(fields[5]));
        }
    }
    None
}

/// An environment backed by a map, for tests and embedding.
#[derive(Debug, Clone, Default)]
pub struct MapEnv {
    vars: HashMap<String, OsString>,
    users: HashMap<String, PathBuf>,
    prefix: Option<PathBuf>,
}

impl MapEnv {
    /// Create from variable pairs.
    pub fn new<K: Into<String>, V: Into<OsString>>(vars: impl IntoIterator<Item = (K, V)>) -> Self {
        MapEnv {
            vars: vars
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            ..Default::default()
        }
    }

    /// Set a variable.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<OsString>) -> &mut Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Add a user home directory.
    pub fn with_user(mut self, user: &str, home: impl Into<PathBuf>) -> Self {
        self.users.insert(user.into(), home.into());
        self
    }

    /// Set the runtime prefix.
    pub fn with_prefix(mut self, prefix: impl Into<PathBuf>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }
}

impl Environment for MapEnv {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).cloned()
    }

    fn user_home(&self, user: &str) -> Option<PathBuf> {
        self.users.get(user).cloned()
    }

    fn runtime_prefix(&self) -> Option<PathBuf> {
        self.prefix.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwd() {
        let data = b"root:x:0:0:root:/root:/bin/bash\nu:x:1000:1000::/home/u:/bin/sh\n";
        assert_eq!(passwd_home(data, "u"), Some(PathBuf::from("/home/u")));
        assert_eq!(passwd_home(data, "nobody"), None);
    }
}
