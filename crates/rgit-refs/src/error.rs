// SPDX-License-Identifier: MIT
//! Error types.

use std::io;
use std::path::PathBuf;

use rgit_fs::LockError;

use crate::messages;
use crate::packed::PackedError;

/// Error while reading references.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file could not be read.
    #[error("unable to read '{}': {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// `packed-refs` is malformed.
    #[error("{}", messages::packed_error(path.display(), source))]
    Packed {
        /// The file, as the store was given it.
        path: PathBuf,
        /// What is wrong.
        source: PackedError,
    },
    /// A loose reference file has content that is neither an object id nor
    /// `ref: <name>`.
    #[error("{}", messages::ignoring_broken_ref(.0))]
    Broken(String),
    /// Symbolic references nest deeper than [`crate::MAX_SYMREF_DEPTH`] or
    /// form a loop.
    #[error("{}", messages::unable_to_resolve(.0))]
    TooDeep(String),
}

/// Why a reference could not be locked (the text after
/// `cannot lock ref '<name>': `).
#[derive(Debug, thiserror::Error)]
pub enum LockReason {
    /// A reference that must exist (old value given) does not.
    #[error("{}", messages::unable_to_resolve(.0))]
    Unresolvable(String),
    /// The current value differs from the expected one.
    #[error("{}", messages::is_at_but_expected(actual, expected))]
    IsAt {
        /// Current value (object id or symbolic target).
        actual: String,
        /// Expected value.
        expected: String,
    },
    /// The reference must not exist but does.
    #[error("{}", messages::ALREADY_EXISTS)]
    AlreadyExists,
    /// The reference must not exist but is a dangling symbolic reference.
    #[error("{}", messages::DANGLING_SYMREF_EXISTS)]
    DanglingSymrefExists,
    /// A no-deref old-oid check on a dangling symbolic reference.
    #[error("{}", messages::missing_but_expected(.0))]
    MissingButExpected(String),
    /// A symbolic old value was given but the reference is a regular one.
    #[error("{}", messages::expected_symref(.0))]
    ExpectedSymref(String),
    /// "must not exist as a symbolic reference" met a regular reference.
    #[error("{}", messages::ERROR_READING)]
    ErrorReading,
    /// A directory without references is in the way.
    #[error("{}", messages::blocking_directory(dir.display(), name))]
    BlockingDirectory {
        /// The directory.
        dir: PathBuf,
        /// The reference.
        name: String,
    },
    /// An existing reference conflicts with the new name.
    #[error("{}", messages::name_conflict(existing, new))]
    NameConflict {
        /// The existing reference.
        existing: String,
        /// The reference to be created.
        new: String,
    },
    /// Two names of the same transaction conflict (one is a directory
    /// prefix of the other).
    #[error("{}", messages::cannot_process_both(name, other))]
    CannotProcessBoth {
        /// The name being checked.
        name: String,
        /// The conflicting name of the transaction.
        other: String,
    },
    /// The lock file could not be created.
    #[error("{0}")]
    Lock(#[from] LockError),
    /// Reading the current value failed.
    #[error("{0}")]
    Read(Error),
    /// Some other I/O error.
    #[error("{0}")]
    Io(String),
}

/// Error from a reference transaction.
#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    /// A reference name (or a symbolic target) is malformed.
    #[error("{}", messages::bad_name(.0))]
    BadName(String),
    /// The same reference is named twice.
    #[error("{}", messages::duplicate(.0))]
    Duplicate(String),
    /// A reference is updated directly and through a symbolic reference.
    #[error("{}", messages::duplicate_via_symref(name, symref))]
    DuplicateViaSymref {
        /// The reference updated twice.
        name: String,
        /// The symbolic reference through which one update came.
        symref: String,
    },
    /// A symbolic reference is updated whose referent is updated too.
    #[error("{}", messages::duplicate_via_referent(name, referent))]
    DuplicateViaReferent {
        /// The symbolic reference.
        name: String,
        /// Its referent.
        referent: String,
    },
    /// `cannot lock ref '<name>': <reason>`
    #[error("{}", messages::cannot_lock(name, reason))]
    CannotLock {
        /// The name as given in the update.
        name: String,
        /// Why.
        reason: LockReason,
    },
    /// A conflict with a packed reference, found after locking.
    #[error("{}", messages::name_conflict(existing, new))]
    NameConflict {
        /// The existing reference.
        existing: String,
        /// The reference to be created.
        new: String,
    },
    /// Two names of the transaction conflict, found after locking.
    #[error("{}", messages::cannot_process_both(name, other))]
    CannotProcessBoth {
        /// The name being checked.
        name: String,
        /// The conflicting name of the transaction.
        other: String,
    },
    /// The symbolic target differs from the expected one.
    #[error("{}", messages::symref_target_mismatch(name, actual, expected))]
    SymrefMismatch {
        /// The reference.
        name: String,
        /// Current target.
        actual: String,
        /// Expected target.
        expected: String,
    },
    /// `packed-refs` could not be locked.
    #[error("{0}")]
    PackedLock(LockError),
    /// Reading failed.
    #[error("{0}")]
    Read(#[from] Error),
    /// Writing failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The error.
        #[source]
        source: io::Error,
    },
    /// A hook ([`crate::RefHooks`]) rejected the transaction.
    #[error("{0}")]
    Rejected(String),
    /// The transaction was already committed or aborted.
    #[error("{}", messages::TRANSACTION_CLOSED)]
    Closed,
}
