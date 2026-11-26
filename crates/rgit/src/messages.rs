// SPDX-License-Identifier: MIT
//! The texts of [`Error`](crate::Error). Where git has a message for a
//! situation it is used verbatim (taken from the crate that already
//! carries it where possible), so a command line client can print it
//! after `fatal: `.

use bstr::BStr;
use rgit_hash::ObjectId;

/// An operation that needs a work tree in a bare repository.
pub fn must_be_run_in_work_tree() -> String {
    rgit_repo::Error::NoWorkTree.to_string()
}

/// A revision expression that names nothing.
pub fn bad_revision(spec: &[u8]) -> String {
    format!("bad revision '{}'", BStr::new(spec))
}

/// A reference name the rules of `git check-ref-format` reject.
pub fn invalid_ref_name(name: &[u8]) -> String {
    format!("'{}' is not a valid ref name", BStr::new(name))
}

/// An object that is referenced but missing.
pub fn unable_to_read(id: &ObjectId) -> String {
    format!("unable to read {id}")
}

/// The library's own text (git has no such situation).
pub fn unsupported(feature: &str, detail: &str) -> String {
    format!("not supported by the rgit library yet: {feature} ({detail})")
}

/// The library's own text (git is interrupted by a signal instead).
pub const CANCELLED: &str = "the operation was cancelled";

/// A commit that cannot be parsed.
pub fn bad_commit(id: &ObjectId) -> String {
    format!("could not parse commit {id}")
}

/// An object of another type where a commit, tree or blob is needed.
pub fn not_a(id: &ObjectId, kind: &str) -> String {
    format!("object {id} is not a {kind}")
}

/// A tree that cannot be parsed.
pub fn bad_tree(id: &ObjectId) -> String {
    format!("corrupt tree {id}")
}
