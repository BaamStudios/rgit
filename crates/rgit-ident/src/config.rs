// SPDX-License-Identifier: MIT
//! Reading the configuration values this crate needs, with git's errors
//! for unusable values.

use std::ffi::OsString;
use std::fmt;

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, Entry, Environment, Level, Origin, ValueErrorKind};

use crate::messages as msg;

/// A configuration or environment value that cannot be used. The command
/// prints [`Problem::report`] and exits with 128.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    lines: Vec<(Level, String)>,
}

impl Problem {
    /// A single `fatal:` line.
    pub fn fatal(message: impl Into<String>) -> Self {
        Problem {
            lines: vec![(Level::Fatal, message.into())],
        }
    }

    /// The lines to print to stderr, each with its level prefix.
    pub fn report(&self) -> &[(Level, String)] {
        &self.lines
    }

    /// Print [`Problem::report`] to stderr.
    pub fn print(&self) {
        for (level, line) in &self.lines {
            eprintln!("{} {line}", level.prefix());
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.lines.last() {
            Some((_, m)) => f.write_str(m),
            None => Ok(()),
        }
    }
}

impl std::error::Error for Problem {}

/// How a value-less entry (`[user] name`) is reported: `Strict` names the
/// variable (what git does for most variables), `Generic` only the source
/// (what it does for `core.pager`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingStyle {
    Strict,
    Generic,
}

fn missing(e: &Entry, style: MissingStyle) -> Problem {
    let name = e.name.to_str_lossy();
    let fatal = match &e.source.origin {
        Origin::CommandLine => match style {
            MissingStyle::Strict => msg::unable_to_parse_cmdline(&name),
            MissingStyle::Generic => msg::UNABLE_TO_PARSE_CMDLINE.to_owned(),
        },
        origin => match style {
            MissingStyle::Strict => {
                let source = match origin {
                    Origin::File(p) => format!("file '{}'", p.display()),
                    other => other.describe(),
                };
                msg::bad_config_variable(&name, &source, e.line)
            }
            MissingStyle::Generic => msg::bad_config_line(e.line, &origin.describe()),
        },
    };
    Problem {
        lines: vec![
            (Level::Error, msg::missing_value(&name)),
            (Level::Fatal, fatal),
        ],
    }
}

/// The last value of a string variable; a value-less entry is an error.
pub fn string(set: &ConfigSet, name: &str) -> Result<Option<BString>, Problem> {
    string_with(set, name, MissingStyle::Strict)
}

/// [`string`] with a chosen report for value-less entries.
pub fn string_with(
    set: &ConfigSet,
    name: &str,
    style: MissingStyle,
) -> Result<Option<BString>, Problem> {
    match set.get(name) {
        None => Ok(None),
        Some(e) => match &e.value {
            Some(v) => Ok(Some(v.clone())),
            None => Err(missing(e, style)),
        },
    }
}

/// Check every entry of `name` (git reads all of them in order and stops
/// at the first bad one) and return the last value.
pub fn string_all(set: &ConfigSet, name: &str) -> Result<Option<BString>, Problem> {
    let mut last = None;
    for e in set.get_all(name) {
        match &e.value {
            Some(v) => last = Some(v.clone()),
            None => return Err(missing(e, MissingStyle::Strict)),
        }
    }
    Ok(last)
}

/// The last value of a boolean variable.
pub fn bool(set: &ConfigSet, name: &str) -> Result<Option<bool>, Problem> {
    let mut last = None;
    for e in set.get_all(name) {
        last = Some(e.bool().map_err(|err| Problem::fatal(err.to_string()))?);
    }
    Ok(last)
}

/// The last value of a path variable, with `~` expanded.
pub fn path(
    set: &ConfigSet,
    name: &str,
    env: &dyn Environment,
) -> Result<Option<std::path::PathBuf>, Problem> {
    let mut last = None;
    for e in set.get_all(name) {
        match e.path(env) {
            Ok(Some(p)) => last = Some(p),
            Ok(None) => {}
            Err(err) if err.kind == ValueErrorKind::Missing => {
                return Err(missing(e, MissingStyle::Strict));
            }
            Err(err) => return Err(Problem::fatal(err.to_string())),
        }
    }
    Ok(last)
}

/// A boolean environment variable as git reads it: unset is `default`, an
/// unparsable value is fatal.
pub fn env_bool(env: &dyn Environment, name: &str, default: bool) -> Result<bool, Problem> {
    let Some(v) = env.var_bytes(name) else {
        return Ok(default);
    };
    rgit_config::value::parse_bool(Some(&v))
        .ok_or_else(|| Problem::fatal(msg::bad_env_bool(&v.to_str_lossy(), name)))
}

/// An environment variable as bytes, keeping an empty value.
pub(crate) fn env_bytes(env: &dyn Environment, name: &str) -> Option<BString> {
    env.var_bytes(name)
}

/// An environment variable as a string (lossy), keeping an empty value.
pub(crate) fn env_string(env: &dyn Environment, name: &str) -> Option<String> {
    env.var(name)
        .map(|v: OsString| v.to_string_lossy().into_owned())
}
