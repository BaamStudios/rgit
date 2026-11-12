// SPDX-License-Identifier: MIT
//! Messages about objects and object names, shared by the plumbing
//! commands (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub fn invalid_object_name(rev: &str) -> String {
    format!("invalid object name '{rev}'.")
}

pub fn not_valid_object_name(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub fn path_not_in_rev(path: &str, rev: &str) -> String {
    format!("path '{path}' does not exist in '{rev}'")
}

pub fn path_on_disk_not_in_rev(path: &str, rev: &str) -> String {
    format!("path '{path}' exists on disk, but not in '{rev}'")
}

pub fn path_not_in_index(path: &str) -> String {
    format!("path '{path}' does not exist (neither on disk nor in the index)")
}

pub fn path_on_disk_not_in_index(path: &str) -> String {
    format!("path '{path}' exists on disk, but not in the index")
}

/// Two lines: the error and a hint naming the stage that exists.
pub fn path_not_at_stage(path: &str, stage: u8, have: u8) -> String {
    format!(
        "path '{path}' is in the index, but not at stage {stage}\n\
         hint: Did you mean ':{have}:{path}'?"
    )
}

pub fn peel_mismatch(
    name: &str,
    want: rgit_object::ObjectKind,
    got: rgit_object::ObjectKind,
) -> String {
    format!("{name}: expected {want} type, but the object dereferences to {got} type")
}

pub fn reflog_too_short(name: &str, n: usize) -> String {
    format!("log for '{name}' only has {n} entries")
}

pub fn short_id_ambiguous(short: &str) -> String {
    format!("short object ID {short} is ambiguous")
}

pub const CANDIDATES_ARE: &str = "The candidates are:";

pub fn candidate_tag(abbrev: &str, date: &str, name: &str) -> String {
    format!("{abbrev} tag {date} - {name}")
}

pub fn candidate_commit(abbrev: &str, date: &str, subject: &str) -> String {
    format!("{abbrev} commit {date} - {subject}")
}

pub fn candidate_other(abbrev: &str, kind: &str) -> String {
    format!("{abbrev} {kind}")
}

pub fn candidate_bad(abbrev: &str) -> String {
    format!("{abbrev} [bad object]")
}

pub fn invalid_object_type(name: &str) -> String {
    format!("invalid object type \"{name}\"")
}

pub fn loose_object_corrupt(oid: &str, path: &str) -> String {
    format!("loose object {oid} (stored in {path}) is corrupt")
}

pub fn could_not_read_from_stdin(err: &str) -> String {
    format!("could not read from stdin: {err}")
}

pub fn unable_to_write_object(kind: &str) -> String {
    format!("unable to write {kind} object")
}
