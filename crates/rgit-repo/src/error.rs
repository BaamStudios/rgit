// SPDX-License-Identifier: MIT
//! Errors of repository setup.

use std::path::PathBuf;

pub use rgit_config::Level;

use crate::messages;

/// Why discovery found no repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotFound {
    /// Reached the root or a `GIT_CEILING_DIRECTORIES` entry.
    Ceiling,
    /// Would have crossed into another file system at this directory.
    FilesystemBoundary(PathBuf),
}

/// A problem with a `.git` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitfileError {
    /// Neither a directory nor a regular file (FIFO, socket, device).
    NotRegular,
    /// Could not be read.
    Read,
    /// Does not start with `gitdir: `.
    InvalidFormat,
    /// `gitdir: ` with nothing after it.
    NoPath,
    /// Points to something that is not a repository.
    NotARepository,
}

/// Errors of repository setup. [`Error::report`] gives the lines git
/// prints; all of them end the command with exit code 128.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No repository was found.
    #[error("{}", match .0 {
        NotFound::Ceiling => messages::NOT_A_REPO_PARENTS.to_string(),
        NotFound::FilesystemBoundary(p) => messages::not_a_repo_mount(p.display()),
    })]
    NotFound(NotFound),
    /// `GIT_DIR` is not a repository; the value as given.
    #[error("{}", messages::not_a_repo_named(.0.display()))]
    NotAGitDir(PathBuf),
    /// A bad `.git` file; the path as git shows it.
    #[error("{}", match .kind {
        GitfileError::NotRegular => messages::not_regular_file(.path.display()),
        GitfileError::Read => messages::error_reading(.path.display()),
        GitfileError::InvalidFormat => messages::invalid_gitfile(.path.display()),
        GitfileError::NoPath => messages::no_path_in_gitfile(.path.display()),
        GitfileError::NotARepository => messages::gitfile_not_repo(.path.display()),
    })]
    Gitfile {
        /// What is wrong.
        kind: GitfileError,
        /// The `.git` file.
        path: PathBuf,
    },
    /// The repository is owned by someone else and not listed in
    /// `safe.directory`.
    #[error("{}", messages::dubious_ownership(.path.display()))]
    DubiousOwnership {
        /// The repository (work tree, or git directory when bare).
        path: PathBuf,
        /// Warnings printed before the error (without `warning: `).
        warnings: Vec<String>,
    },
    /// An implicitly found bare repository with
    /// `safe.bareRepository=explicit`.
    #[error("{}", messages::bare_not_allowed(.0.display()))]
    BareNotAllowed(PathBuf),
    /// `core.repositoryFormatVersion` is newer than 1.
    #[error("{}", messages::repo_version_too_new(*.0))]
    VersionTooNew(i64),
    /// Version 1 with extensions this implementation does not know.
    #[error("{}", messages::unknown_extensions(.0))]
    UnknownExtensions(Vec<String>),
    /// Version 0 with extensions that require version 1.
    #[error("{}", messages::v1_only_extensions(.0))]
    V1OnlyExtensions(Vec<String>),
    /// A known extension rgit does not implement yet (rgit's own message).
    #[error("{}", messages::unsupported(.0))]
    Unsupported(String),
    /// A configuration entry with a value git rejects while reading the
    /// repository format: `error: <message>` then
    /// `fatal: bad config line <line> in <source>`.
    #[error("{}", .fatal)]
    BadEntry {
        /// The `error:` line.
        message: String,
        /// The `fatal:` line.
        fatal: String,
    },
    /// A protected configuration value git cannot parse.
    #[error("{0}")]
    BadProtectedValue(String),
    /// Reading configuration failed.
    #[error(transparent)]
    Config(#[from] rgit_config::Error),
    /// A configuration value of the wrong type.
    #[error(transparent)]
    Value(#[from] rgit_config::ValueError),
    /// A work tree path that cannot be resolved.
    #[error("{}", messages::invalid_path(.path.display(), crate::error::io_text(.source)))]
    InvalidPath {
        /// The path up to the failing component.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// `core.worktree` names a directory that cannot be entered.
    #[error("{}", messages::cannot_chdir(.path.display(), crate::error::io_text(.source)))]
    CannotChdir {
        /// The configured value.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// `GIT_WORK_TREE` or `--work-tree` is empty.
    #[error("{}", messages::EMPTY_PATH)]
    EmptyWorkTree,
    /// The command needs a work tree and there is none.
    #[error("{}", messages::MUST_RUN_IN_WORK_TREE)]
    NoWorkTree,
    /// The command needs a work tree but `core.bare` and `core.worktree`
    /// conflict.
    #[error("{}", messages::INVALID_WORKTREE_CONFIG)]
    InvalidWorkTreeConfig,
    /// The current directory cannot be determined.
    #[error("unable to get current working directory")]
    Cwd(#[source] std::io::Error),
}

impl Error {
    /// The lines git prints for this error, in order. The last one is the
    /// `fatal:` line.
    pub fn report(&self) -> Vec<(Level, String)> {
        match self {
            Error::Config(e) => e.report(),
            Error::DubiousOwnership { warnings, .. } => {
                let mut out: Vec<_> = warnings
                    .iter()
                    .map(|w| (Level::Warning, w.clone()))
                    .collect();
                out.push((Level::Fatal, self.to_string()));
                out
            }
            Error::BadEntry { message, fatal } => {
                vec![
                    (Level::Error, message.clone()),
                    (Level::Fatal, fatal.clone()),
                ]
            }
            _ => vec![(Level::Fatal, self.to_string())],
        }
    }

    /// Whether this is [`Error::NotFound`].
    pub fn is_not_found(&self) -> bool {
        matches!(self, Error::NotFound(_))
    }
}

impl From<crate::path::InvalidPath> for Error {
    fn from(e: crate::path::InvalidPath) -> Self {
        Error::InvalidPath {
            path: e.path,
            source: e.source,
        }
    }
}

/// The C library's text for an I/O error.
pub(crate) fn io_text(err: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match err.kind() {
        NotFound => "No such file or directory".into(),
        PermissionDenied => "Permission denied".into(),
        NotADirectory => "Not a directory".into(),
        _ => {
            let s = err.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        }
    }
}
