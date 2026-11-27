// SPDX-License-Identifier: MIT
//! References for rgit: names, loose files, `packed-refs`, symbolic
//! references and atomic transactions.
//!
//! * [`name`]: the `check-ref-format` rules.
//! * [`RefStore`]: the backend interface (read, resolve, iterate,
//!   transactions). [`files::FilesStore`] implements it for loose files and
//!   `packed-refs`, and forwards to the reftable backend in a repository
//!   whose git directory has a `reftable/` directory.
//! * [`reftable`]: the reftable format (tables, the stack of tables,
//!   compaction) and the backend on top of it.
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
pub mod location;
pub mod loose;
pub mod messages;
pub mod name;
pub mod packed;
pub mod reflog;
pub mod reftable;
mod store;
mod target;

pub use error::{Error, LockReason, TransactionError};
pub use store::{
    HookState, IterItem, IterOptions, LogAllRefUpdates, MAX_SYMREF_DEPTH, NewValue, NoHooks,
    OldValue, PreparedTransaction, RefHooks, RefStore, RefUpdate, ReflogUpdate, Transaction,
    TransactionState, check_duplicates, check_update_name,
};
pub use target::{Peeled, Reference, Resolved, Source, Target};

/// The content of `HEAD` of the git directory `git_dir` in the files
/// format (`ref: <name>` or an object id, with a line feed): the file
/// itself, or in a reftable repository the value stored in the stack.
pub fn read_head_file(git_dir: &std::path::Path) -> std::io::Result<Vec<u8>> {
    let git_dir = &location::resolve(git_dir);
    if !reftable::backend::uses_reftable(git_dir) {
        return std::fs::read(rgit_fs::path::join(git_dir, "HEAD"));
    }
    reftable::backend::root_ref_content(git_dir, "HEAD")
}

/// Whether the state file `name` of a git directory is a reference that
/// a reftable repository keeps in its stack: every root reference except
/// `HEAD` and the pseudo-references `FETCH_HEAD` and `MERGE_HEAD`
/// (`ORIG_HEAD`, `CHERRY_PICK_HEAD`, `AUTO_MERGE`, ...). Other names
/// (`MERGE_MSG`, ...) are plain files in either format.
pub fn is_stack_root_ref(name: &str) -> bool {
    name != "HEAD" && name::is_root_ref(name) && !name::is_pseudo_ref(name)
}

/// The content of the root reference `name` (`ORIG_HEAD`, ...) of the git
/// directory `git_dir` in the files format: the file of that name, or in
/// a reftable repository the value stored in the stack. A name that is
/// not stored in the stack ([`is_stack_root_ref`]) is read as a file in
/// either format.
pub fn read_root_ref_file(git_dir: &std::path::Path, name: &str) -> std::io::Result<Vec<u8>> {
    let git_dir = &location::resolve(git_dir);
    if is_stack_root_ref(name) && reftable::backend::uses_reftable(git_dir) {
        return reftable::backend::root_ref_content(git_dir, name);
    }
    std::fs::read(rgit_fs::path::join(git_dir, name))
}

/// [`read_root_ref_file`] as text (bytes that are not UTF-8 escaped as
/// [`rgit_bstr`] does).
pub fn read_root_ref_text(git_dir: &std::path::Path, name: &str) -> std::io::Result<String> {
    Ok(rgit_bstr::from_vec(read_root_ref_file(git_dir, name)?))
}

/// Whether the root reference `name` of `git_dir` exists (see
/// [`read_root_ref_file`]).
pub fn root_ref_exists(git_dir: &std::path::Path, name: &str) -> bool {
    read_root_ref_file(git_dir, name).is_ok()
}

/// Stores `content` (`ref: <name>` or an object id in hex, as the files
/// format writes it) as the root reference `name` of `git_dir`, without a
/// reflog entry: as a file, or in a reftable repository in the stack. A
/// name that is not stored in the stack ([`is_stack_root_ref`]) is
/// written as a file in either format.
pub fn write_root_ref_file(
    git_dir: &std::path::Path,
    name: &str,
    content: &[u8],
) -> std::io::Result<()> {
    let git_dir = &location::resolve(git_dir);
    if !is_stack_root_ref(name) || !reftable::backend::uses_reftable(git_dir) {
        return std::fs::write(rgit_fs::path::join(git_dir, name), content);
    }
    let text = rgit_bstr::from_bytes(content);
    let text = text.trim();
    let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidData);
    let update = match text.strip_prefix("ref:") {
        Some(target) => RefUpdate::symref_update(name, target.trim(), None),
        None => {
            let id = rgit_hash::ObjectId::from_hex_any(text.as_bytes()).map_err(|_| invalid())?;
            RefUpdate::update(name, id, None)
        }
    };
    root_ref_transaction(git_dir, update)
}

/// Removes the root reference `name` of `git_dir`: the file, or in a
/// reftable repository the value in the stack. Removing a reference that
/// does not exist is not an error.
pub fn remove_root_ref_file(git_dir: &std::path::Path, name: &str) -> std::io::Result<()> {
    let git_dir = &location::resolve(git_dir);
    if is_stack_root_ref(name) && reftable::backend::uses_reftable(git_dir) {
        if reftable::backend::root_ref_content(git_dir, name).is_err() {
            return Ok(());
        }
        let kind = reftable::backend::stack_hash(git_dir).unwrap_or(rgit_hash::HashKind::Sha1);
        return root_ref_transaction(git_dir, RefUpdate::delete(name, kind, None));
    }
    match std::fs::remove_file(rgit_fs::path::join(git_dir, name)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn root_ref_transaction(git_dir: &std::path::Path, update: RefUpdate) -> std::io::Result<()> {
    let kind = reftable::backend::stack_hash(git_dir).unwrap_or(rgit_hash::HashKind::Sha1);
    let store = files::FilesStore::new(git_dir, kind);
    let mut tx = Transaction::new(&store);
    tx.push(update.no_deref(true).skip_name_check(true))
        .and_then(|()| tx.commit(&mut NoHooks))
        .map_err(std::io::Error::other)
}

/// [`read_head_file`] as text (bytes that are not UTF-8 escaped as
/// [`rgit_bstr`] does).
pub fn read_head_text(git_dir: &std::path::Path) -> std::io::Result<String> {
    Ok(rgit_bstr::from_vec(read_head_file(git_dir)?))
}

/// Points `HEAD` of the reftable repository or worktree in `git_dir` at
/// the reference `target`, without a reflog entry. Returns `false`, and
/// does nothing, when the directory uses the files format (the caller
/// writes the file).
pub fn write_head_symref(git_dir: &std::path::Path, target: &str) -> std::io::Result<bool> {
    let git_dir = &location::resolve(git_dir);
    if !reftable::backend::uses_reftable(git_dir) {
        return Ok(false);
    }
    root_ref_transaction(git_dir, RefUpdate::symref_update("HEAD", target, None))?;
    Ok(true)
}
