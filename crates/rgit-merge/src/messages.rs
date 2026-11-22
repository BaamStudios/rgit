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
pub const TYPE_RENAME_DELETE: &str = "CONFLICT (rename/delete)";
pub const TYPE_RENAME_RENAME: &str = "CONFLICT (rename/rename)";
pub const TYPE_DIRECTORY_RENAME_SUGGESTED: &str = "CONFLICT (directory rename suggested)";
pub const TYPE_PATH_UPDATED: &str = "Path updated due to directory rename";
pub const TYPE_DIRECTORY_RENAME_SPLIT: &str = "CONFLICT(directory rename unclear split)";
pub const TYPE_DIRECTORY_RENAME_COLLISION: &str = "CONFLICT(directory rename collision)";
pub const TYPE_DIRECTORY_RENAME_SKIPPED: &str =
    "Directory rename skipped since directory was renamed on both sides";
pub const TYPE_RENAME_COLLISION: &str = "CONFLICT (rename involved in collision)";
pub const TYPE_FILE_IN_THE_WAY: &str = "CONFLICT (file in way of directory rename)";
pub const TYPE_SUBMODULE_FAST_FORWARD: &str = "Fast forwarding submodule";
pub const TYPE_SUBMODULE: &str = "CONFLICT (submodule)";

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

pub fn submodule_failed(path: &[u8]) -> Vec<u8> {
    join(&[b"Failed to merge submodule ", path])
}

pub fn submodule_fast_forward(path: &[u8], to: &str) -> Vec<u8> {
    join(&[
        b"Note: Fast-forwarding submodule ",
        path,
        b" to ",
        to.as_bytes(),
    ])
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

pub fn rename_delete(old: &[u8], new: &[u8], renamed_in: &[u8], deleted_in: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (rename/delete): ",
        old,
        b" renamed to ",
        new,
        b" in ",
        renamed_in,
        b", but deleted in ",
        deleted_in,
        b".",
    ])
}

pub fn rename_rename(old: &[u8], new1: &[u8], side1: &[u8], new2: &[u8], side2: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (rename/rename): ",
        old,
        b" renamed to ",
        new1,
        b" in ",
        side1,
        b" and to ",
        new2,
        b" in ",
        side2,
        b".",
    ])
}

/// How a path came into a directory the other side renamed: added, or
/// renamed from another path.
fn arrived(path: &[u8], renamed_from: Option<&[u8]>, side: &[u8]) -> Vec<u8> {
    match renamed_from {
        None => join(&[path, b" added in ", side, b" inside"]),
        Some(old) => join(&[old, b" renamed to ", path, b" in ", side, b", inside"]),
    }
}

/// `merge.directoryRenames=conflict`: the path was moved, with a conflict.
pub fn file_location(
    path: &[u8],
    renamed_from: Option<&[u8]>,
    side: &[u8],
    dir_renamed_in: &[u8],
    new_path: &[u8],
) -> Vec<u8> {
    join(&[
        b"CONFLICT (file location): ",
        &arrived(path, renamed_from, side),
        b" a directory that was renamed in ",
        dir_renamed_in,
        b", suggesting it should perhaps be moved to ",
        new_path,
        b".",
    ])
}

/// `merge.directoryRenames=true`: the path was moved.
pub fn path_updated(
    path: &[u8],
    renamed_from: Option<&[u8]>,
    side: &[u8],
    dir_renamed_in: &[u8],
    new_path: &[u8],
) -> Vec<u8> {
    join(&[
        b"Path updated: ",
        &arrived(path, renamed_from, side),
        b" a directory that was renamed in ",
        dir_renamed_in,
        b"; moving it to ",
        new_path,
        b".",
    ])
}

pub fn directory_rename_split(dir: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (directory rename split): Unclear where to rename ",
        dir,
        b" to; it was renamed to multiple other directories, with no destination getting a majority of the files.",
    ])
}

pub fn directory_rename_collision(new_path: &[u8], paths: &[&[u8]]) -> Vec<u8> {
    join(&[
        b"CONFLICT (implicit dir rename): Cannot map more than one path to ",
        new_path,
        b"; implicit directory renames tried to put these paths there: ",
        &paths.join(&b", "[..]),
    ])
}

pub fn file_in_the_way(new_path: &[u8], paths: &[&[u8]]) -> Vec<u8> {
    join(&[
        b"CONFLICT (implicit dir rename): Existing file/dir at ",
        new_path,
        b" in the way of implicit directory rename(s) putting the following path(s) there: ",
        &paths.join(&b", "[..]),
        b".",
    ])
}

pub fn directory_rename_skipped(old: &[u8], new: &[u8], path: &[u8]) -> Vec<u8> {
    join(&[
        b"WARNING: Avoiding applying ",
        old,
        b" -> ",
        new,
        b" rename to ",
        path,
        b", because ",
        new,
        b" itself was renamed.",
    ])
}

pub fn rename_collision(old: &[u8], new: &[u8]) -> Vec<u8> {
    join(&[
        b"CONFLICT (rename involved in collision): rename of ",
        old,
        b" -> ",
        new,
        b" has content conflicts AND collides with another path; this may result in nested conflict markers.",
    ])
}

/// The warnings when the exhaustive rename search was skipped.
pub fn rename_limit_warnings(needed: usize) -> [String; 2] {
    [
        "warning: exhaustive rename detection was skipped due to too many files.".to_owned(),
        format!(
            "warning: you may want to set your merge.renamelimit variable to at least {needed} and retry the command."
        ),
    ]
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
