// SPDX-License-Identifier: MIT
//! Reflogs for rgit (`git-reflog(1)`, `gitrevisions(7)`).
//!
//! * [`Entry`]: one line `<old> <new> <name> <<email>> <time> <tz>\t<message>`,
//!   parsed with git's tolerance and written byte for byte like git.
//!   [`normalize_message`] folds a message onto one line.
//! * [`FilesReflog`]: the files below `logs/`: read, append (creating the
//!   file and its directories when asked), create empty, delete with the
//!   reference, rename with a branch, list.
//! * [`LogAllRefUpdates`]: `core.logAllRefUpdates`, which updates create a
//!   missing log.
//! * [`lookup`]: `@{<n>}`, `@{<date>}` and `@{-<n>}` for the revision
//!   parser, with git's warnings.
//! * [`ReflogBackend`] / [`ReflogWriter`]: what a reference transaction
//!   calls to log an update or drop the log of a deleted reference.
//!
//! ```
//! use rgit_hash::HashKind;
//! use rgit_reflog::{Committer, FilesReflog};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let dir = tempfile::tempdir()?;
//! let logs = FilesReflog::new(dir.path(), dir.path(), HashKind::Sha1);
//! let me = Committer::new(b"C O Mitter", b"c@example.com", 1112911993, -700);
//! let id = "d76260c32be1d2ada5abb7f70e6bdd385ac72493".parse()?;
//! logs.log_update("refs/heads/main", HashKind::Sha1.null(), id, &me, b"branch: Created from HEAD", true)?;
//! assert_eq!(logs.read("refs/heads/main")?.unwrap().len(), 1);
//! # Ok(())
//! # }
//! ```

mod entry;
mod files;
pub mod lookup;
pub mod messages;
mod name;

use std::path::PathBuf;

use rgit_hash::ObjectId;

pub use entry::{Committer, Entry, normalize_message};
pub use files::FilesReflog;
pub use lookup::{LookupError, Resolved, Selector, Target, Warning, nth_prior_checkout, resolve};
pub use name::{LogAllRefUpdates, is_per_worktree, is_valid_refname};

/// Errors writing reflogs. `Display` gives git's message without the
/// `fatal: ` prefix (a reference transaction puts
/// `cannot update the ref '<name>': ` in front).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Opening or writing the log failed.
    #[error("{}", messages::unable_to_append(.0, .1))]
    Append(PathBuf, String),
    /// A non-empty directory is where the log file has to go.
    #[error("{}", messages::logs_under(.0))]
    LogsUnder(PathBuf),
    /// Moving a log (branch rename) failed.
    #[error("{}", messages::unable_to_rename(.0, .1, .2))]
    Rename(PathBuf, PathBuf, String),
}

/// The reflog side of a reference transaction: the transaction decides
/// what to log (old and new value, message, whether `core.logAllRefUpdates`
/// or `--create-reflog` asks for a new log) and calls this while the
/// reference is locked.
pub trait ReflogBackend {
    /// Appends one entry to the log of `name`; with `create`, a missing
    /// log is created first. The message is normalised. An `Err` carries
    /// the message for `cannot update the ref '<name>': <message>`.
    fn reflog_append(
        &mut self,
        name: &str,
        old: ObjectId,
        new: ObjectId,
        message: &str,
        create: bool,
    ) -> Result<(), String>;

    /// Removes the log of a deleted reference.
    fn reflog_delete(&mut self, name: &str);
}

/// [`ReflogBackend`] for the files backend with a fixed committer
/// (identity and date of the running command).
#[derive(Clone, Debug)]
pub struct ReflogWriter {
    /// Where the logs are.
    pub store: FilesReflog,
    /// Who makes the updates, and when.
    pub committer: Committer,
}

impl ReflogWriter {
    /// A writer appending entries by `committer` to `store`.
    pub fn new(store: FilesReflog, committer: Committer) -> Self {
        ReflogWriter { store, committer }
    }
}

impl ReflogBackend for ReflogWriter {
    fn reflog_append(
        &mut self,
        name: &str,
        old: ObjectId,
        new: ObjectId,
        message: &str,
        create: bool,
    ) -> Result<(), String> {
        self.store
            .log_update(name, old, new, &self.committer, message.as_bytes(), create)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn reflog_delete(&mut self, name: &str) {
        let _ = self.store.delete(name);
    }
}
