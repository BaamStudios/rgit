// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git merge-file` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const MERGE_FILE_USAGE: &[&str] = &[
    "git merge-file [<options>] [-L <name1> [-L <orig> [-L <name2>]]] <file1> <orig-file> <file2>",
];
pub const HELP_STDOUT: &str = "send results to standard output";
pub const HELP_OBJECT_ID: &str = "use object IDs instead of filenames";
pub const HELP_DIFF3: &str = "use a diff3 based merge";
pub const HELP_ZDIFF3: &str = "use a zealous diff3 based merge";
pub const HELP_OURS: &str = "for conflicts, use our version";
pub const HELP_THEIRS: &str = "for conflicts, use their version";
pub const HELP_UNION: &str = "for conflicts, use a union version";
pub const HELP_DIFF_ALGORITHM: &str = "choose a diff algorithm";
pub const HELP_MARKER_SIZE: &str = "for conflicts, use this marker size";
pub const HELP_QUIET: &str = "do not warn about conflicts";
pub const HELP_LABEL: &str = "set labels for file1/orig-file/file2";
pub const TOO_MANY_LABELS: &str = "too many labels on the command line";
pub const BAD_DIFF_ALGORITHM: &str =
    "option diff-algorithm accepts \"myers\", \"minimal\", \"patience\" and \"histogram\"";

pub fn could_not_stat(path: &str, err: &str) -> String {
    format!("Could not stat {path}: {err}")
}

pub fn could_not_open(path: &str, err: &str) -> String {
    format!("Could not open {path}: {err}")
}

pub fn could_not_open_for_writing(path: &str, err: &str) -> String {
    format!("Could not open {path} for writing: {err}")
}

pub fn cannot_merge_binary(name: &str) -> String {
    format!("Cannot merge binary files: {name}")
}

pub fn object_does_not_exist(name: &str) -> String {
    format!("object '{name}' does not exist")
}

pub fn unable_to_read_blob(oid: &str) -> String {
    format!("unable to read blob object {oid}")
}

pub fn unknown_conflict_style(value: &str) -> String {
    format!("unknown style '{value}' given for 'merge.conflictstyle'")
}

pub fn unable_to_write_object() -> String {
    "Unable to add merge result to the object database".to_string()
}

pub fn algorithm_not_supported(name: &str) -> String {
    format!("rgit merge-file: --diff-algorithm={name} is not supported yet")
}
