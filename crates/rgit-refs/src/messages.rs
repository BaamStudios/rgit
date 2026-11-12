// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, collected in one place for review.
//!
//! The wording reproduces what the reference `git` binary (2.56) prints in
//! the same situations, as observed by running it (see CLEANROOM.md, "User-
//! visible messages"). Callers add `fatal: `, `error: ` or `warning: `.

use std::fmt::Display;

/// `refusing to update ref with bad name '<name>'`
pub fn bad_name(name: &str) -> String {
    format!("refusing to update ref with bad name '{name}'")
}

/// `multiple updates for ref '<name>' not allowed`
pub fn duplicate(name: &str) -> String {
    format!("multiple updates for ref '{name}' not allowed")
}

/// A reference is updated directly and through a symbolic reference.
pub fn duplicate_via_symref(name: &str, symref: &str) -> String {
    format!("multiple updates for '{name}' (including one via symref '{symref}') are not allowed")
}

/// A symbolic reference is updated whose referent is already updated.
pub fn duplicate_via_referent(name: &str, referent: &str) -> String {
    format!(
        "multiple updates for '{name}' (including one via its referent '{referent}') are not allowed"
    )
}

/// `cannot lock ref '<name>': <reason>`
pub fn cannot_lock(name: &str, reason: impl Display) -> String {
    format!("cannot lock ref '{name}': {reason}")
}

/// `unable to resolve reference '<name>'`
pub fn unable_to_resolve(name: &str) -> String {
    format!("unable to resolve reference '{name}'")
}

/// `is at <actual> but expected <expected>`
pub fn is_at_but_expected(actual: impl Display, expected: impl Display) -> String {
    format!("is at {actual} but expected {expected}")
}

/// The reference must not exist but does.
pub const ALREADY_EXISTS: &str = "reference already exists";

/// The reference must not exist but is a dangling symbolic reference.
pub const DANGLING_SYMREF_EXISTS: &str = "dangling symref already exists";

/// A symbolic-reference expectation met a regular reference.
pub const ERROR_READING: &str = "error reading reference";

/// `reference is missing but expected <oid>`
pub fn missing_but_expected(expected: impl Display) -> String {
    format!("reference is missing but expected {expected}")
}

/// `expected symref with target '<target>': but is a regular ref`
pub fn expected_symref(target: &str) -> String {
    format!("expected symref with target '{target}': but is a regular ref")
}

/// `verifying symref target: '<name>': is at <actual> but expected <target>`
pub fn symref_target_mismatch(name: &str, actual: &str, expected: &str) -> String {
    format!("verifying symref target: '{name}': is at {actual} but expected {expected}")
}

/// `'<existing>' exists; cannot create '<new>'`
pub fn name_conflict(existing: &str, new: &str) -> String {
    format!("'{existing}' exists; cannot create '{new}'")
}

/// `cannot process '<a>' and '<b>' at the same time`
pub fn cannot_process_both(a: &str, b: &str) -> String {
    format!("cannot process '{a}' and '{b}' at the same time")
}

/// A directory without references sits where the reference file goes.
pub fn blocking_directory(dir: impl Display, name: &str) -> String {
    format!("there is a non-empty directory '{dir}' blocking reference '{name}'")
}

/// `unexpected line in <path>: <line>` / `unterminated line in <path>: <line>`
pub fn packed_error(path: impl Display, error: &crate::packed::PackedError) -> String {
    use crate::packed::PackedError;
    match error {
        PackedError::UnexpectedLine(line) => format!("unexpected line in {path}: {line}"),
        PackedError::UnterminatedLine(line) => format!("unterminated line in {path}: {line}"),
    }
}

/// `ignoring broken ref <name>` (printed as a warning while iterating).
pub fn ignoring_broken_ref(name: &str) -> String {
    format!("ignoring broken ref {name}")
}

/// `ignoring ref with broken name <name>` (printed as a warning while
/// iterating).
pub fn ignoring_broken_name(name: &str) -> String {
    format!("ignoring ref with broken name {name}")
}

/// `trying to write ref '<name>' with nonexistent object <oid>`
pub fn nonexistent_object(name: &str, oid: impl Display) -> String {
    format!("trying to write ref '{name}' with nonexistent object {oid}")
}

/// `trying to write non-commit object <oid> to branch '<name>'`
pub fn non_commit_on_branch(oid: impl Display, name: &str) -> String {
    format!("trying to write non-commit object {oid} to branch '{name}'")
}

/// `cannot update ref '<name>': couldn't write '<lock>'`: the same
/// reference was reached twice through symbolic references.
pub fn couldnt_write(name: &str, lock_path: &str) -> String {
    format!("cannot update ref '{name}': couldn't write '{lock_path}'")
}

/// Usage of `git check-ref-format`.
pub const CHECK_REF_FORMAT_USAGE: &str = "\
usage: git check-ref-format [--normalize] [<options>] <refname>
   or: git check-ref-format --branch <branchname-shorthand>
";

/// `'<name>' is not a valid branch name` (`check-ref-format --branch`).
pub fn invalid_branch_name(name: &str) -> String {
    format!("'{name}' is not a valid branch name")
}

/// The transaction was used after commit or abort.
pub const TRANSACTION_CLOSED: &str = "transaction is closed";
