// SPDX-License-Identifier: MIT
//! Configuration errors of this crate, reported the way git reports them.

use bstr::{BStr, BString, ByteSlice};
use rgit_config::{Entry, Level, Origin, ValueError};

use crate::messages;

/// A configuration value this crate could not use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A value-less key where a string is required (`remote.x.url`).
    #[error("{}", messages::bad_config_variable(.name.as_bstr(), .origin, *.line))]
    MissingValue {
        /// Canonical variable name.
        name: BString,
        /// Where it was set.
        origin: Origin,
        /// 1-based line (0 for the command line).
        line: usize,
    },
    /// A value of the wrong type (for example `bad boolean config value`).
    #[error(transparent)]
    Value(#[from] ValueError),
    /// A value outside the accepted set
    /// (`unknown value for config 'protocol.allow': x`).
    #[error("{}", messages::unknown_config_value(.name, .value.as_bstr()))]
    UnknownValue {
        /// Variable name as git prints it.
        name: String,
        /// The value.
        value: BString,
    },
    /// An invalid `push.default`.
    #[error("{}", messages::bad_config_variable(b"push.default".as_bstr(), .origin, *.line))]
    PushDefault {
        /// The value.
        value: BString,
        /// Where it was set.
        origin: Origin,
        /// 1-based line.
        line: usize,
    },
}

impl ConfigError {
    pub(crate) fn missing(entry: &Entry) -> ConfigError {
        ConfigError::MissingValue {
            name: entry.name.clone(),
            origin: entry.source.origin.clone(),
            line: entry.line,
        }
    }

    /// The lines git prints, in order (the last one is `fatal:`).
    pub fn report(&self) -> Vec<(Level, String)> {
        match self {
            ConfigError::MissingValue { name, origin, .. } => {
                let last = if matches!(origin, Origin::CommandLine) {
                    messages::unable_to_parse_cmdline(name.as_bstr())
                } else {
                    self.to_string()
                };
                vec![
                    (Level::Error, messages::missing_value(name.as_bstr())),
                    (Level::Fatal, last),
                ]
            }
            ConfigError::PushDefault { value, origin, .. } => {
                let [a, b] = messages::malformed_push_default(value.as_bstr());
                let last = if matches!(origin, Origin::CommandLine) {
                    messages::unable_to_parse_cmdline(b"push.default".as_bstr())
                } else {
                    self.to_string()
                };
                vec![(Level::Error, a), (Level::Error, b), (Level::Fatal, last)]
            }
            _ => vec![(Level::Fatal, self.to_string())],
        }
    }
}

/// The string value of `entry`, or the missing-value error.
pub(crate) fn string_value(entry: &Entry) -> Result<&BStr, ConfigError> {
    entry
        .value
        .as_deref()
        .map(|v| v.as_bstr())
        .ok_or_else(|| ConfigError::missing(entry))
}
