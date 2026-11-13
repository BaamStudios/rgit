// SPDX-License-Identifier: MIT

//! User-visible messages of this crate, verbatim as git prints them (see
//! CLEANROOM.md, "User-visible messages"). Callers add the `error: `
//! prefix where git prints one.

/// A merge (`-m`) with unmerged entries in the index (`fatal: `).
pub const RESOLVE_INDEX_FIRST: &str = "You need to resolve your current index first";

/// Printed by porcelain commands after the grouped refusal messages.
pub const ABORTING: &str = "Aborting";

/// A tree that cannot be read (`fatal: `).
pub fn failed_to_unpack(id: &str) -> String {
    format!("failed to unpack tree object {id}")
}

/// A tree entry whose path may not be checked out (`error: `).
pub fn invalid_path(path: &str) -> String {
    format!("invalid path '{path}'")
}

/// Plumbing (`read-tree`): the index entry differs from the old and the new
/// tree.
pub fn would_overwrite(path: &str) -> String {
    format!("Entry '{path}' would be overwritten by merge. Cannot merge.")
}

/// Plumbing: the work tree file differs from its index entry.
pub fn not_uptodate_file(path: &str) -> String {
    format!("Entry '{path}' not uptodate. Cannot merge.")
}

/// Plumbing: a directory to be replaced by a file holds untracked files.
pub fn not_uptodate_dir(path: &str) -> String {
    format!("Updating '{path}' would lose untracked files in it")
}

/// Plumbing: an untracked file stands where a tracked one is written.
pub fn untracked_overwritten(path: &str) -> String {
    format!("Untracked working tree file '{path}' would be overwritten by merge.")
}

/// Porcelain `checkout`/`switch`: local changes (header, list, trailer).
pub const CHECKOUT_LOCAL_CHANGES: (&str, &str) = (
    "Your local changes to the following files would be overwritten by checkout:\n",
    "Please commit your changes or stash them before you switch branches.",
);

/// Porcelain `merge`: local changes.
pub const MERGE_LOCAL_CHANGES: (&str, &str) = (
    "Your local changes to the following files would be overwritten by merge:\n",
    "Please commit your changes or stash them before you merge.",
);

/// Porcelain: directories with untracked files (no trailer; git ends the
/// block with an empty line).
pub const DIRS_WITH_UNTRACKED: (&str, &str) = (
    "Updating the following directories would lose untracked files in them:\n",
    "",
);

/// Porcelain `checkout`: untracked files in the way.
pub const CHECKOUT_UNTRACKED_OVERWRITTEN: (&str, &str) = (
    "The following untracked working tree files would be overwritten by checkout:\n",
    "Please move or remove them before you switch branches.",
);

/// Porcelain `merge`: untracked files in the way.
pub const MERGE_UNTRACKED_OVERWRITTEN: (&str, &str) = (
    "The following untracked working tree files would be overwritten by merge:\n",
    "Please move or remove them before you merge.",
);

/// `checkout-index`: the path has no index entry.
pub fn not_in_cache(path: &str) -> String {
    format!("git checkout-index: {path} is not in the cache")
}

/// `checkout-index`: the path only has conflict stages.
pub fn unmerged(path: &str) -> String {
    format!("git checkout-index: {path} is unmerged")
}

/// `checkout-index`: a different file exists and `-f` was not given.
pub fn already_exists(path: &str) -> String {
    format!("{path} already exists, no checkout")
}

/// A leading directory cannot be created because a file is there
/// (`fatal: `).
pub fn cannot_create_directory(path: &str, err: &str) -> String {
    format!("cannot create directory at '{path}': {err}")
}

/// Writing a file failed (`error: `).
pub fn unable_to_create_file(path: &str, err: &str) -> String {
    format!("unable to create file {path}: {err}")
}

/// Creating a symbolic link failed (`error: `).
pub fn unable_to_create_symlink(path: &str, err: &str) -> String {
    format!("unable to create symlink {path}: {err}")
}

/// Removing a file failed (`warning: `).
pub fn unable_to_unlink(path: &str, err: &str) -> String {
    format!("unable to unlink '{path}': {err}")
}

/// A blob to be checked out is missing (`error: `).
pub fn unable_to_read_sha1(id: &str) -> String {
    format!("unable to read sha1 file of {id}")
}
