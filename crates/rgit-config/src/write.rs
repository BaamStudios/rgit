// SPDX-License-Identifier: MIT
//! Applying [`crate::edit`] operations to a file through `<file>.lock`.
//!
//! The file is locked first, then read and edited; the new content is
//! written to the lock file, which gets the old file's permissions and is
//! renamed over the file. A missing file counts as empty and is created.
//! Missing directories are not created (git does not either).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rgit_fs::{LockError, LockFile, LockOptions};

use crate::edit::{self, EditError, SetRequest};
use crate::error::Level;
use crate::messages;

/// Failure of a file edit. [`WriteError::report`] gives git's messages,
/// [`WriteError::exit_code`] its exit code.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// `<file>.lock` could not be created.
    #[error("{}", messages::could_not_lock(.path.display(), &lock_io(.source)))]
    Lock {
        /// The file as named by the caller.
        path: PathBuf,
        /// What went wrong.
        source: LockError,
        /// rename/remove-section print the message without the cause.
        terse: bool,
    },
    /// The file exists but could not be read.
    #[error("{}", messages::opening(.path.display(), .source))]
    Read {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
        /// rename/remove-section only warn.
        terse: bool,
    },
    /// The edit itself failed.
    #[error("{source}")]
    Edit {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: EditError,
    },
    /// Writing or renaming the lock file failed.
    #[error("{}", if *.commit {
        messages::could_not_commit(.path.display(), .source)
    } else {
        messages::could_not_write(.path.display(), .source)
    })]
    Write {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
        /// The rename failed (rather than the write).
        commit: bool,
    },
}

fn lock_io(e: &LockError) -> std::io::Error {
    match e {
        LockError::Exists { .. } => std::io::Error::from(std::io::ErrorKind::AlreadyExists),
        LockError::Io { source, .. } => std::io::Error::new(source.kind(), source.to_string()),
    }
}

impl WriteError {
    /// The underlying edit error, if that is what failed.
    pub fn edit_error(&self) -> Option<&EditError> {
        match self {
            WriteError::Edit { source, .. } => Some(source),
            _ => None,
        }
    }

    /// The lines git prints, in order, without their prefixes.
    pub fn report(&self) -> Vec<(Level, String)> {
        match self {
            WriteError::Lock {
                path, terse: true, ..
            } => vec![(
                Level::Error,
                format!("could not lock config file {}", path.display()),
            )],
            WriteError::Read {
                path,
                source,
                terse,
            } => {
                let warning = messages::unable_to_access(path.display(), source);
                if *terse {
                    vec![(Level::Warning, warning)]
                } else if source.kind() == std::io::ErrorKind::IsADirectory {
                    vec![
                        (Level::Warning, warning),
                        (
                            Level::Error,
                            format!("invalid config file {}", path.display()),
                        ),
                    ]
                } else {
                    vec![(Level::Error, self.to_string())]
                }
            }
            WriteError::Edit { path, source } => match source {
                EditError::Syntax { line } => vec![(
                    Level::Fatal,
                    messages::bad_config_line(*line, format!("file {}", path.display())),
                )],
                EditError::OverlongLine { line } => {
                    vec![(Level::Error, messages::overlong_line(path.display(), *line))]
                }
                EditError::MultipleValues { .. } => vec![(Level::Warning, source.to_string())],
                EditError::NothingToUnset => vec![],
                EditError::MultilineComment(_) | EditError::NoSuchSection(_) => {
                    vec![(Level::Fatal, source.to_string())]
                }
                _ => vec![(Level::Error, source.to_string())],
            },
            _ => vec![(Level::Error, self.to_string())],
        }
    }

    /// git's exit code.
    pub fn exit_code(&self) -> i32 {
        match self {
            WriteError::Lock { .. } | WriteError::Write { .. } => 255,
            WriteError::Read { terse: true, .. } => 255,
            WriteError::Read { .. } => 3,
            WriteError::Edit {
                source: EditError::Syntax { .. },
                ..
            } => 128,
            WriteError::Edit { source, .. } => source.exit_code(),
        }
    }
}

/// Lock `path`, read it (missing = empty), let `edit` compute the new
/// content and commit it with the file's permissions kept.
pub fn edit_file(
    path: &Path,
    lock: &LockOptions,
    terse: bool,
    edit: impl FnOnce(&[u8]) -> Result<Vec<u8>, EditError>,
) -> Result<(), WriteError> {
    let mut lockfile = LockFile::acquire(path, lock).map_err(|source| WriteError::Lock {
        path: path.to_owned(),
        source,
        terse,
    })?;
    let target = lockfile.target().to_owned();
    let (data, mode) = match std::fs::read(&target) {
        Ok(d) => (d, std::fs::metadata(&target).ok().map(|m| m.permissions())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
        Err(source) => {
            return Err(WriteError::Read {
                path: path.to_owned(),
                source,
                terse,
            });
        }
    };
    let new = edit(&data).map_err(|source| WriteError::Edit {
        path: path.to_owned(),
        source,
    })?;
    let write_err = |source, commit| WriteError::Write {
        path: path.to_owned(),
        source,
        commit,
    };
    if let Some(perm) = mode {
        lockfile
            .file_mut()
            .set_permissions(perm)
            .map_err(|e| write_err(e, false))?;
    }
    lockfile.write_all(&new).map_err(|e| write_err(e, false))?;
    lockfile.commit().map_err(|e| write_err(e, true))
}

/// Set, add, replace or unset values in `path` (see [`edit::set`]).
pub fn set_in_file(
    path: &Path,
    lock: &LockOptions,
    req: &SetRequest<'_>,
) -> Result<(), WriteError> {
    // Argument errors come before any file access.
    let early = |source| WriteError::Edit {
        path: path.to_owned(),
        source,
    };
    crate::Key::parse(req.key).map_err(|e| early(e.into()))?;
    if let Some(c) = req.comment {
        edit::format_comment(c).map_err(early)?;
    }
    edit_file(path, lock, false, |data| edit::set(data, req))
}

/// Rename (`new = Some`) or remove a section in `path` (see
/// [`edit::rename_section`]).
pub fn rename_section_in_file(
    path: &Path,
    lock: &LockOptions,
    old: &[u8],
    new: Option<&[u8]>,
) -> Result<(), WriteError> {
    edit_file(path, lock, true, |data| {
        edit::rename_section(data, old, new)
    })
}
