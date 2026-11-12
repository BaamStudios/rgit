// SPDX-License-Identifier: MIT
//! Reflogs (`git-reflog(1)`, `gitrevisions(7)`).
//!
//! * [`Entry`]: one line `<old> <new> <name> <<email>> <time> <tz>\t<message>`,
//!   parsed with git's tolerance and written byte for byte like git.
//!   [`normalize_message`] folds a message onto one line.
//! * [`FilesReflog`]: the files below `logs/`: read, append (creating the
//!   file and its directories when asked), create empty, delete with the
//!   reference, rename with a branch, list.
//! * [`LogAllRefUpdates`](crate::LogAllRefUpdates) decides which updates
//!   create a missing log; the transaction passes that on as
//!   [`ReflogUpdate::create`].
//! * [`lookup`]: `@{<n>}`, `@{<date>}` and `@{-<n>}` for the revision
//!   parser, with git's warnings.
//! * [`ReflogWriter`]: the [`RefHooks`] that make a [`Transaction`](crate::Transaction)
//!   write reflogs.
//!
//! ```
//! use rgit_hash::HashKind;
//! use rgit_refs::reflog::{Committer, FilesReflog};
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

use std::path::PathBuf;

pub use entry::{Committer, Entry, normalize_message};
pub use files::FilesReflog;
pub use lookup::{LookupError, Selector, nth_prior_checkout, resolve};

use crate::store::{RefHooks, ReflogUpdate};

/// Errors writing reflogs. `Display` gives git's message without the
/// `fatal: ` prefix (a reference transaction puts
/// `cannot update the ref '<name>': ` in front).
#[derive(Debug, thiserror::Error)]
pub enum ReflogError {
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

/// Writes the reflog entries of a reference transaction, all by one
/// committer (identity and date of the running command).
///
/// Use it as the transaction's [`RefHooks`], or call it from hooks that
/// also do other work.
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

impl RefHooks for ReflogWriter {
    fn reflog_append(&mut self, update: &ReflogUpdate) -> Result<(), String> {
        self.store
            .log_update(
                &update.name,
                update.old,
                update.new,
                &self.committer,
                update.message.as_bytes(),
                update.create,
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn reflog_delete(&mut self, name: &str) {
        let _ = self.store.delete(name);
    }
}
