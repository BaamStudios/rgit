// SPDX-License-Identifier: MIT
//! Errors of configuration loading and value parsing.

use std::fmt;
use std::path::PathBuf;

use bstr::BString;

use crate::key::KeyError;
use crate::messages;
use crate::origin::Origin;

/// Severity of one reported line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `warning: ...`
    Warning,
    /// `error: ...`
    Error,
    /// `fatal: ...` (the last line; the command exits with 128)
    Fatal,
}

impl Level {
    /// The prefix git prints, without the trailing space.
    pub fn prefix(self) -> &'static str {
        match self {
            Level::Warning => "warning:",
            Level::Error => "error:",
            Level::Fatal => "fatal:",
        }
    }
}

/// A configuration error.
///
/// git often reports one problem with several lines (an `error:` with the
/// cause followed by `fatal: bad config line N in file X`).
/// [`Error::report`] returns those lines in order; `Display` shows only the
/// final one.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Syntax error in a file or blob.
    #[error("{}", messages::bad_config_line(*.line, .origin.describe()))]
    Syntax {
        /// 1-based line.
        line: usize,
        /// Where.
        origin: Origin,
    },
    /// An entry was syntactically fine but could not be processed (for
    /// example an include with a missing value).
    #[error("{}", match .origin {
        Origin::CommandLine => messages::UNABLE_TO_PARSE_COMMAND_LINE.to_string(),
        o => messages::bad_config_line(*.line, o.describe()),
    })]
    Entry {
        /// The underlying problem.
        cause: Box<Error>,
        /// Line of the entry.
        line: usize,
        /// Where.
        origin: Origin,
    },
    /// A value-less variable where a value is required.
    #[error("{}", messages::missing_value(.name.as_ref()))]
    MissingValue {
        /// Canonical variable name.
        name: BString,
    },
    /// `~user` in an include path could not be resolved.
    #[error("{}", messages::expand_include_path(.path.as_ref()))]
    ExpandIncludePath {
        /// The value as written.
        path: BString,
    },
    /// Relative `include.path` outside a file.
    #[error("{}", messages::RELATIVE_INCLUDE)]
    RelativeInclude,
    /// `gitdir:./` outside a file.
    #[error("{}", messages::RELATIVE_INCLUDE_CONDITION)]
    RelativeIncludeCondition,
    /// Too deeply nested includes.
    #[error("{}", messages::include_depth(*.depth, .path.display(), .from))]
    IncludeDepth {
        /// The limit.
        depth: usize,
        /// The file that was to be included.
        path: PathBuf,
        /// The including source, as text.
        from: String,
    },
    /// `remote.*.url` in a file included through `hasconfig:remote.*.url`.
    #[error("{}", messages::REMOTE_URL_IN_HASCONFIG)]
    RemoteUrlInHasconfig,
    /// A file exists but cannot be read.
    #[error("{}", messages::unable_to_access(.path.display(), .source))]
    Access {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
        /// git warns before dying for repository files.
        warn: bool,
    },
    /// Invalid command-line configuration (`-c`, `GIT_CONFIG_*`).
    #[error("{}", messages::UNABLE_TO_PARSE_COMMAND_LINE)]
    CommandLine(String),
    /// Invalid variable name.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// Invalid `--config-env` argument: git dies with this message.
    #[error("{0}")]
    ConfigEnv(String),
    /// A value that does not parse as the requested type.
    #[error(transparent)]
    Value(#[from] ValueError),
}

impl Error {
    /// The lines git prints for this error, in order.
    pub fn report(&self) -> Vec<(Level, String)> {
        match self {
            Error::Entry { cause, .. } => {
                let mut out: Vec<_> = cause
                    .report()
                    .into_iter()
                    .map(|(l, m)| (if l == Level::Fatal { Level::Error } else { l }, m))
                    .collect();
                out.push((Level::Fatal, self.to_string()));
                out
            }
            Error::CommandLine(msg) => vec![
                (Level::Error, msg.clone()),
                (Level::Fatal, self.to_string()),
            ],
            Error::Access { warn: true, .. } => vec![
                (Level::Warning, self.to_string()),
                (Level::Fatal, self.to_string()),
            ],
            _ => vec![(Level::Fatal, self.to_string())],
        }
    }

    /// Errors git raises with `die()` immediately, without the
    /// `bad config line` context.
    pub(crate) fn is_immediate(&self) -> bool {
        matches!(
            self,
            Error::IncludeDepth { .. }
                | Error::RemoteUrlInHasconfig
                | Error::Access { .. }
                | Error::Entry { .. }
                | Error::Syntax { .. }
        )
    }

    /// Wrap an error raised while processing an entry.
    pub(crate) fn in_entry(self, line: usize, origin: &Origin) -> Error {
        if self.is_immediate() {
            return self;
        }
        let cause = match self {
            Error::CommandLine(msg) => return Error::CommandLine(msg),
            Error::Key(k) if matches!(origin, Origin::CommandLine) => {
                return Error::CommandLine(k.to_string());
            }
            other => other,
        };
        Error::Entry {
            cause: Box::new(cause),
            line,
            origin: origin.clone(),
        }
    }
}

/// A value does not parse as the requested type. `Display` gives git's
/// message, e.g. `bad numeric config value 'x' for 'a.b' in file f: invalid unit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError {
    /// What was expected.
    pub kind: ValueErrorKind,
    /// Canonical variable name.
    pub name: BString,
    /// The value (empty for a value-less key).
    pub value: BString,
    /// The origin, when known.
    pub origin: Option<Origin>,
}

/// The type a value failed to parse as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueErrorKind {
    /// `bool`
    Bool,
    /// `int` with the reason.
    Int(crate::value::IntError),
    /// A required value is missing (value-less key).
    Missing,
    /// `path`: `~user` expansion failed.
    Path,
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use bstr::ByteSlice;
        let name = self.name.as_bstr();
        let value = self.value.as_bstr();
        match &self.kind {
            ValueErrorKind::Bool => f.write_str(&messages::bad_boolean(value, name)),
            ValueErrorKind::Int(e) => {
                let source = match &self.origin {
                    None | Some(Origin::CommandLine) => None,
                    Some(o) => Some(o.describe()),
                };
                f.write_str(&messages::bad_numeric(value, name, source, e.reason()))
            }
            ValueErrorKind::Missing => f.write_str(&messages::missing_value(name)),
            ValueErrorKind::Path => f.write_str(&messages::expand_user_dir(value)),
        }
    }
}

impl std::error::Error for ValueError {}
