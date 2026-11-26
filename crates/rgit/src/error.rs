// SPDX-License-Identifier: MIT
//! The error type of the library API (ADR 0005, "Errors").

use std::path::PathBuf;

use bstr::BString;
use rgit_hash::ObjectId;

use crate::messages;

/// The error of a lower rgit crate, kept as the source of an [`Error`].
///
/// The concrete types behind it belong to the plumbing crates and are not
/// part of the API of this crate.
pub type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// How serious a [`Diagnostic`] is; a command line client prints the
/// matching prefix (`hint: `, `warning: `, `error: `).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Level {
    /// Advice on what to do next.
    Hint,
    /// Something the caller should know; the operation went on.
    Warning,
    /// A problem that led to the error it is attached to.
    Error,
}

impl Level {
    /// The prefix git prints, with the colon and the space.
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Level::Hint => "hint: ",
            Level::Warning => "warning: ",
            Level::Error => "error: ",
        }
    }
}

/// A message the library would print if it were a program: returned to
/// the caller instead, who decides what to do with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How serious it is.
    pub level: Level,
    /// The text, without a prefix and without a trailing line feed.
    pub text: String,
}

impl Diagnostic {
    pub(crate) fn warning(text: impl Into<String>) -> Diagnostic {
        Diagnostic {
            level: Level::Warning,
            text: text.into(),
        }
    }
}

/// The message as git prints it, without the final line feed: the prefix
/// of the level, on every line for a hint.
impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.level != Level::Hint {
            return write!(f, "{}{}", self.level.prefix(), self.text);
        }
        for (i, line) in self.text.split('\n').enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            if line.is_empty() {
                f.write_str("hint:")?;
            } else {
                write!(f, "hint: {line}")?;
            }
        }
        Ok(())
    }
}

/// What went wrong in a call of the library.
///
/// The library never prints and never ends the process. The text of an
/// error ([`std::fmt::Display`]) is the message git prints after `fatal: `
/// wherever git has one for the situation, so a command line client can
/// print [`Error::diagnostics`], then `fatal: {error}`, and exit with
/// [`Error::exit_code`].
///
/// New variants can be added in minor releases.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// There is no repository at (or, for a discovery, above) `path`.
    #[error("{message}")]
    NotARepository {
        /// The directory given to the call.
        path: PathBuf,
        /// git's message for the situation.
        message: String,
    },
    /// A repository was found but cannot be used: unknown format version
    /// or extension, a directory owned by someone else (`safe.directory`),
    /// a broken `.git` file.
    #[error("{message}")]
    Open {
        /// The directory given to the call.
        path: PathBuf,
        /// git's message for the problem.
        message: String,
        /// What git prints before the message.
        diagnostics: Vec<Diagnostic>,
    },
    /// The configuration cannot be read or holds a value that is not
    /// valid for its key.
    #[error("{message}")]
    Config {
        /// git's message for the problem.
        message: String,
    },
    /// The operation needs a work tree and the repository is bare.
    #[error("{}", messages::must_be_run_in_work_tree())]
    BareRepository,
    /// A revision expression names nothing.
    #[error("{}", messages::bad_revision(.spec))]
    RevisionNotFound {
        /// The expression.
        spec: BString,
    },
    /// A revision expression cannot be resolved for a reason git reports
    /// on its own: an ambiguous short id, a reflog that is too short, a
    /// branch without upstream, an object of the wrong type.
    #[error("{message}")]
    Revision {
        /// The expression.
        spec: BString,
        /// git's message for the problem.
        message: String,
        /// What git prints before the message (the candidates of an
        /// ambiguous short id, for example).
        diagnostics: Vec<Diagnostic>,
    },
    /// A reference name that is not valid, or one the reference backend
    /// cannot represent yet (names that are not UTF-8, until the backend
    /// takes byte strings).
    #[error("{}", messages::invalid_ref_name(.name))]
    InvalidRefName {
        /// The name.
        name: BString,
    },
    /// An object a reference or another object points to is missing.
    #[error("{}", messages::unable_to_read(.id))]
    ObjectNotFound {
        /// The object.
        id: ObjectId,
    },
    /// An object, the index or a reference is damaged or cannot be read.
    #[error("{message}")]
    Corrupt {
        /// git's message for the problem.
        message: String,
        /// The error of the crate that read the data.
        #[source]
        source: Option<Source>,
    },
    /// A file system operation failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The error of the operating system.
        #[source]
        source: std::io::Error,
    },
    /// The repository uses something this version of the library cannot
    /// do. Returned instead of an answer that could be wrong.
    #[error("{}", messages::unsupported(.feature, .detail))]
    Unsupported {
        /// The missing feature.
        feature: &'static str,
        /// What needed it (a path, a configuration key).
        detail: String,
    },
    /// The caller cancelled the operation through its
    /// [`Cancel`](crate::Cancel) token.
    #[error("{}", messages::CANCELLED)]
    Cancelled,
}

impl Error {
    /// The exit code a command line client uses for this error: git's 128
    /// for everything it reports with `fatal:`, and 130 (interrupted) for a
    /// cancelled operation.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Cancelled => 130,
            _ => 128,
        }
    }

    /// What git prints before the `fatal:` line. Empty for most errors.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Error::Open { diagnostics, .. } | Error::Revision { diagnostics, .. } => diagnostics,
            _ => &[],
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
        Error::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn corrupt(source: impl std::error::Error + Send + Sync + 'static) -> Error {
        Error::Corrupt {
            message: source.to_string(),
            source: Some(Box::new(source)),
        }
    }

    pub(crate) fn corrupt_message(message: impl Into<String>) -> Error {
        Error::Corrupt {
            message: message.into(),
            source: None,
        }
    }

    pub(crate) fn config(e: impl std::fmt::Display) -> Error {
        Error::Config {
            message: e.to_string(),
        }
    }
}

/// The result type of the library API.
pub type Result<T, E = Error> = std::result::Result<T, E>;
