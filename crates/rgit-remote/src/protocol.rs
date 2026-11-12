// SPDX-License-Identifier: MIT
//! Which transport protocols may be used: `protocol.allow`,
//! `protocol.<name>.allow`, `GIT_ALLOW_PROTOCOL` and
//! `GIT_PROTOCOL_FROM_USER` (`git-config(1)`, `git(1)`).

use bstr::{BStr, BString, ByteSlice};
use rgit_config::{ConfigSet, Environment};

use crate::error::{ConfigError, string_value};
use crate::messages;

/// A protocol policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Always usable.
    Always,
    /// Never usable.
    Never,
    /// Usable unless `GIT_PROTOCOL_FROM_USER` is false.
    User,
}

impl Policy {
    fn parse(value: &[u8]) -> Option<Policy> {
        if value.eq_ignore_ascii_case(b"always") {
            Some(Policy::Always)
        } else if value.eq_ignore_ascii_case(b"never") {
            Some(Policy::Never)
        } else if value.eq_ignore_ascii_case(b"user") {
            Some(Policy::User)
        } else {
            None
        }
    }
}

/// Why a protocol may not be used. `Display` is git's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// The policy forbids it: `transport '<name>' not allowed`.
    #[error("{}", messages::transport_not_allowed(.0.as_bstr()))]
    NotAllowed(BString),
    /// A bad `protocol.*` value.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `GIT_PROTOCOL_FROM_USER` is not a boolean.
    #[error("{}", messages::bad_env_bool(.0.as_bstr(), FROM_USER_VAR))]
    BadEnvironment(BString),
}

const ALLOW_VAR: &str = "GIT_ALLOW_PROTOCOL";
const FROM_USER_VAR: &str = "GIT_PROTOCOL_FROM_USER";

/// The policy for `protocol` (as named in `protocol.<name>.allow`).
pub fn policy(
    protocol: &[u8],
    config: &ConfigSet,
    env: &dyn Environment,
) -> Result<Policy, ConfigError> {
    if let Some(list) = env.var_bytes(ALLOW_VAR) {
        let allowed = list.split(|&c| c == b':').any(|p| p == protocol);
        return Ok(if allowed {
            Policy::Always
        } else {
            Policy::Never
        });
    }
    let mut key = BString::from("protocol.");
    key.extend_from_slice(protocol);
    key.extend_from_slice(b".allow");
    for name in [key.as_bstr(), b"protocol.allow".as_bstr()] {
        if let Some(e) = config.get(name) {
            let v = string_value(e)?;
            return Policy::parse(v).ok_or_else(|| ConfigError::UnknownValue {
                name: name.to_str_lossy().into_owned(),
                value: v.to_owned(),
            });
        }
    }
    Ok(match protocol {
        b"http" | b"https" | b"git" | b"ssh" => Policy::Always,
        b"ext" => Policy::Never,
        _ => Policy::User,
    })
}

/// Whether `protocol` may be used now.
pub fn is_allowed(
    protocol: &[u8],
    config: &ConfigSet,
    env: &dyn Environment,
) -> Result<bool, ProtocolError> {
    Ok(match policy(protocol, config, env)? {
        Policy::Always => true,
        Policy::Never => false,
        Policy::User => match env.var_bytes(FROM_USER_VAR) {
            None => true,
            Some(v) => rgit_config::value::parse_bool(Some(&v))
                .ok_or_else(|| ProtocolError::BadEnvironment(v.clone()))?,
        },
    })
}

/// Fail with `transport '<name>' not allowed` unless `protocol` may be
/// used.
pub fn check_allowed(
    protocol: &[u8],
    config: &ConfigSet,
    env: &dyn Environment,
) -> Result<(), ProtocolError> {
    if is_allowed(protocol, config, env)? {
        Ok(())
    } else {
        Err(ProtocolError::NotAllowed(BStr::new(protocol).to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{MapEnv, Origin, Scope, parse_bytes};

    fn cfg(text: &str) -> ConfigSet {
        parse_bytes(text.as_bytes(), Origin::Stdin, Scope::Local).unwrap()
    }

    fn env(vars: &[(&str, &str)]) -> MapEnv {
        MapEnv::new(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())))
    }

    fn check(proto: &str, c: &ConfigSet, e: &MapEnv) -> String {
        match check_allowed(proto.as_bytes(), c, e) {
            Ok(()) => "ok".into(),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn defaults_and_overrides() {
        let none = cfg("");
        let e = env(&[]);
        assert_eq!(check("ssh", &none, &e), "ok");
        assert_eq!(check("file", &none, &e), "ok");
        assert_eq!(check("ext", &none, &e), "transport 'ext' not allowed");
        let user0 = env(&[("GIT_PROTOCOL_FROM_USER", "0")]);
        assert_eq!(check("file", &none, &user0), "transport 'file' not allowed");
        assert_eq!(check("https", &none, &user0), "ok");
        let bad = env(&[("GIT_PROTOCOL_FROM_USER", "bogus")]);
        assert_eq!(
            check("file", &none, &bad),
            "bad boolean environment value 'bogus' for 'GIT_PROTOCOL_FROM_USER'"
        );
        let c = cfg("[protocol]\n allow = never\n[protocol \"file\"]\n allow = ALWAYS\n");
        assert_eq!(check("file", &c, &e), "ok");
        assert_eq!(check("ssh", &c, &e), "transport 'ssh' not allowed");
        let c = cfg("[protocol \"FILE\"]\n allow = never\n");
        assert_eq!(check("file", &c, &e), "ok");
        let c = cfg("[protocol]\n allow = bogus\n");
        assert_eq!(
            check("file", &c, &e),
            "unknown value for config 'protocol.allow': bogus"
        );
        let c = cfg("[protocol \"file\"]\n allow = bogus\n");
        assert_eq!(
            check("file", &c, &e),
            "unknown value for config 'protocol.file.allow': bogus"
        );
    }

    #[test]
    fn allow_env() {
        let c = cfg("[protocol \"file\"]\n allow = never\n");
        let e = env(&[("GIT_ALLOW_PROTOCOL", ":file:")]);
        assert_eq!(check("file", &c, &e), "ok");
        assert_eq!(check("ssh", &c, &e), "transport 'ssh' not allowed");
        let empty = env(&[("GIT_ALLOW_PROTOCOL", "")]);
        assert_eq!(check("file", &c, &empty), "transport 'file' not allowed");
    }
}
