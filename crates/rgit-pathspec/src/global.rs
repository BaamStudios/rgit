// SPDX-License-Identifier: MIT

//! Global pathspec settings: `--literal-pathspecs`, `--glob-pathspecs`,
//! `--noglob-pathspecs`, `--icase-pathspecs` and their environment variables.

use crate::{Magic, PathspecError};

/// Environment variable for `--literal-pathspecs`.
pub const ENV_LITERAL: &str = "GIT_LITERAL_PATHSPECS";
/// Environment variable for `--glob-pathspecs`.
pub const ENV_GLOB: &str = "GIT_GLOB_PATHSPECS";
/// Environment variable for `--noglob-pathspecs`.
pub const ENV_NOGLOB: &str = "GIT_NOGLOB_PATHSPECS";
/// Environment variable for `--icase-pathspecs`.
pub const ENV_ICASE: &str = "GIT_ICASE_PATHSPECS";

/// The four global pathspec switches.
///
/// The CLI sets the environment variables for the command line options (so
/// that subprocesses inherit them) and the library reads them with
/// [`GlobalFlags::from_env`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlobalFlags {
    /// No globbing and no magic at all.
    pub literal: bool,
    /// Add `glob` magic to every pathspec (unless `literal` is given).
    pub glob: bool,
    /// Add `literal` magic to every pathspec (unless `glob` is given).
    pub noglob: bool,
    /// Add `icase` magic to every pathspec.
    pub icase: bool,
}

impl GlobalFlags {
    /// Reads the four environment variables of the current process.
    ///
    /// # Errors
    /// [`PathspecError::BadBoolEnv`] for a value that is not a boolean.
    pub fn from_env() -> Result<Self, PathspecError> {
        Self::from_lookup(|name| std::env::var_os(name).map(|v| v.into_encoded_bytes()))
    }

    /// Like [`GlobalFlags::from_env`] with a custom variable lookup.
    ///
    /// # Errors
    /// [`PathspecError::BadBoolEnv`] for a value that is not a boolean.
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<Vec<u8>>,
    ) -> Result<Self, PathspecError> {
        let mut get = |name: &'static str| -> Result<bool, PathspecError> {
            match lookup(name) {
                None => Ok(false),
                Some(value) => {
                    parse_env_bool(&value).ok_or(PathspecError::BadBoolEnv { name, value })
                }
            }
        };
        Ok(Self {
            literal: get(ENV_LITERAL)?,
            glob: get(ENV_GLOB)?,
            noglob: get(ENV_NOGLOB)?,
            icase: get(ENV_ICASE)?,
        })
    }

    /// Checks that the combination is allowed.
    ///
    /// # Errors
    /// [`PathspecError::GlobalGlobAndNoglob`] or
    /// [`PathspecError::GlobalLiteralAndOther`].
    pub fn validate(&self) -> Result<(), PathspecError> {
        if self.glob && self.noglob {
            return Err(PathspecError::GlobalGlobAndNoglob);
        }
        if self.literal && (self.glob || self.noglob || self.icase) {
            return Err(PathspecError::GlobalLiteralAndOther);
        }
        Ok(())
    }

    /// Applies the global settings to the magic of one parsed pathspec.
    pub(crate) fn apply(&self, mut magic: Magic) -> Magic {
        if self.glob && !magic.contains(Magic::LITERAL) {
            magic |= Magic::GLOB;
        }
        if self.noglob && !magic.contains(Magic::GLOB) {
            magic |= Magic::LITERAL;
        }
        if self.icase {
            magic |= Magic::ICASE;
        }
        magic
    }
}

/// git's boolean environment syntax: `true`/`yes`/`on`, `false`/`no`/`off`,
/// the empty string (false) or an integer (non-zero is true), with an
/// optional `k`/`m`/`g` unit suffix.
pub(crate) fn parse_env_bool(value: &[u8]) -> Option<bool> {
    let lower = value.to_ascii_lowercase();
    match lower.as_slice() {
        b"true" | b"yes" | b"on" => return Some(true),
        b"false" | b"no" | b"off" | b"" => return Some(false),
        _ => {}
    }
    let (num, _unit) = match lower.last() {
        Some(b'k' | b'm' | b'g') => (&lower[..lower.len() - 1], lower.last()),
        _ => (&lower[..], None),
    };
    let digits = match num.first() {
        Some(b'+' | b'-') => &num[1..],
        _ => num,
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(digits.iter().any(|&d| d != b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_bools() {
        assert_eq!(parse_env_bool(b"1"), Some(true));
        assert_eq!(parse_env_bool(b"0"), Some(false));
        assert_eq!(parse_env_bool(b""), Some(false));
        assert_eq!(parse_env_bool(b"Yes"), Some(true));
        assert_eq!(parse_env_bool(b"off"), Some(false));
        assert_eq!(parse_env_bool(b"2k"), Some(true));
        assert_eq!(parse_env_bool(b"bogus"), None);
    }

    #[test]
    fn combinations() {
        let f = GlobalFlags {
            glob: true,
            noglob: true,
            ..Default::default()
        };
        assert_eq!(f.validate(), Err(PathspecError::GlobalGlobAndNoglob));
        let f = GlobalFlags {
            literal: true,
            icase: true,
            ..Default::default()
        };
        assert_eq!(f.validate(), Err(PathspecError::GlobalLiteralAndOther));
    }
}
