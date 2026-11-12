// SPDX-License-Identifier: MIT
//! References for rgit: names, loose files, `packed-refs`, symbolic
//! references and atomic transactions.
//!
//! * [`name`]: the `check-ref-format` rules.
//! * [`RefStore`]: the backend interface (read, resolve, iterate,
//!   transactions). [`files::FilesStore`] implements it for loose files and
//!   `packed-refs`; reftable (#31) is a second implementation.
//! * [`Transaction`]: queue [`RefUpdate`]s, prepare (lock and check), commit
//!   or abort. Either every update is applied or none.
//! * [`reflog`]: reflog files, `@{n}` / `@{date}` / `@{-n}` lookup, and
//!   [`reflog::ReflogWriter`], the [`RefHooks`] that log a transaction.
//! * [`RefHooks`]: hook points for reflog writes, the
//!   `reference-transaction` hook (#76) and object checks (object database).
//!
//! The on-disk results (loose files, `packed-refs`, removed directories) are
//! byte-identical to what the reference `git` produces for the same updates;
//! see the differential tests in `tests/`.
//!
//! ```no_run
//! use rgit_hash::{HashKind, ObjectId};
//! use rgit_refs::{files::FilesStore, NoHooks, RefStore, RefUpdate, Transaction};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = FilesStore::new(".git", HashKind::Sha1);
//! let id: ObjectId = "b2e6e911d0750ef5d57039fc461069c4a5277897".parse()?;
//! let mut tx = Transaction::new(&store);
//! tx.push(RefUpdate::create("refs/heads/topic", id).message("branch: Created"))?;
//! tx.commit(&mut NoHooks)?;
//! assert_eq!(store.resolve_oid("refs/heads/topic")?, Some(id));
//! # Ok(())
//! # }
//! ```

mod error;
pub mod files;
pub mod loose;
pub mod messages;
pub mod name;
pub mod packed;
pub mod reflog;
mod store;
mod target;

pub use error::{Error, LockReason, TransactionError};
pub use store::{
    HookState, IterItem, IterOptions, LogAllRefUpdates, MAX_SYMREF_DEPTH, NewValue, NoHooks,
    OldValue, PreparedTransaction, RefHooks, RefStore, RefUpdate, ReflogUpdate, Transaction,
    TransactionState, check_duplicates, check_update_name,
};
pub use target::{Peeled, Reference, Resolved, Source, Target};
