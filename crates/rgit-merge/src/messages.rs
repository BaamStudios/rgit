// SPDX-License-Identifier: MIT

//! User-visible strings of the merge engine, verbatim as git prints them
//! (see CLEANROOM.md, "User-visible messages"): the informational messages
//! of a merge, their stable type names (`merge-tree -z`) and the labels git
//! puts after conflict markers.

/// Label of our side in the merges that build a virtual merge base.
pub const TEMPORARY_BRANCH_1: &str = "Temporary merge branch 1";
/// Label of their side in the merges that build a virtual merge base.
pub const TEMPORARY_BRANCH_2: &str = "Temporary merge branch 2";
/// Base label when the merge base is a virtual one built from several.
pub const MERGED_COMMON_ANCESTORS: &str = "merged common ancestors";
/// Base label when the histories have no common ancestor.
pub const EMPTY_TREE: &str = "empty tree";

/// Stable type names of the messages (`merge-tree -z`).
pub const TYPE_AUTO_MERGING: &str = "Auto-merging";
pub const TYPE_CONTENTS: &str = "CONFLICT (contents)";
pub const TYPE_BINARY: &str = "CONFLICT (binary)";
pub const TYPE_FILE_DIRECTORY: &str = "CONFLICT (file/directory)";
pub const TYPE_DISTINCT_MODES: &str = "CONFLICT (distinct modes)";
pub const TYPE_MODIFY_DELETE: &str = "CONFLICT (modify/delete)";
pub const TYPE_SUBMODULE_NOT_INITIALIZED: &str = "CONFLICT (submodule not initialized)";

/// Concatenates byte strings.
fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

pub fn auto_merging(path: &[u8]) -> Vec<u8> {
    join(&[b"Auto-merging ", path])
}

/// A content conflict; `add_add` when the path did not exist in the base.
pub fn content_conflict(path: &[u8], add_add: bool) -> Vec<u8> {
    let kind: &[u8] = if add_add { b"add/add" } else { b"content" };
    join(&[b"CONFLICT (", kind, b"): Merge conflict in ", path])
}

pub fn submodule_conflict(path: &[u8]) -> Vec<u8> {
    join(&[b"CONFLICT (submodule): Merge conflict in ", path])
}

pub fn submodule_not_checked_out(path: &[u8]) -> Vec<u8> {
    join(&[b"Failed to merge submodule ", path, b" (not checked out)"])
}

pub fn binary(path: &[u8], ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    join(&[
        b"warning: Cannot merge binary files: ",
        path,
        b" (",
        ours,
        b" vs. ",
        theirs,
        b")",
    ])
}

pub fn modify_delete(path: &[u8], deleted_in: &[u8], modified_in: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (modify/delete): ",
        path,
        b" deleted in ",
        deleted_in,
        b" and modified in ",
        modified_in,
        b".  Version ",
        modified_in,
        b" of ",
        path,
        b" left in tree.",
    ])
}

pub fn file_directory(path: &[u8], branch: &[u8], new_path: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (file/directory): directory in the way of ",
        path,
        b" from ",
        branch,
        b"; moving it to ",
        new_path,
        b" instead.",
    ])
}

pub fn distinct_types(path: &[u8], both: bool) -> Vec<u8> {
    let which: &[u8] = if both { b"both" } else { b"one" };
    join(&[
        b"CONFLICT (distinct types): ",
        path,
        b" had different types on each side; renamed ",
        which,
        b" of them so each can be recorded somewhere.",
    ])
}

/// Error texts of [`crate::Error`].
pub fn missing_object(id: &str) -> String {
    format!("unable to read object {id}")
}

pub fn not_a_tree(id: &str) -> String {
    format!("unable to read tree ({id})")
}

pub fn not_a_commit(id: &str) -> String {
    format!("object {id} is not a commit")
}

pub fn corrupt_object(id: &str, why: &str) -> String {
    format!("corrupt object {id}: {why}")
}

pub fn write_failed(why: &str) -> String {
    format!("unable to write object: {why}")
}

pub const UNRELATED_HISTORIES: &str = "refusing to merge unrelated histories";
