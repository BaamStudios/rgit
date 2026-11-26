// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git replace` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git replace [-f] <object> <replacement>",
    "git replace [-f] --edit <object>",
    "git replace [-f] --graft <commit> [<parent>...]",
    "git replace [-f] --convert-graft-file",
    "git replace -d <object>...",
    "git replace [--format=<format>] [-l [<pattern>]]",
];

pub const HELP_LIST: &str = "list replace refs";
pub const HELP_DELETE: &str = "delete replace refs";
pub const HELP_EDIT: &str = "edit existing object";
pub const HELP_GRAFT: &str = "change a commit's parents";
pub const HELP_CONVERT: &str = "convert existing graft file";
pub const HELP_FORCE: &str = "replace the ref if it exists";
pub const HELP_RAW: &str = "do not pretty-print contents for --edit";
pub const HELP_FORMAT: &str = "use this format";

pub const FORMAT_NOT_LISTING: &str = "--format cannot be used when not listing";
pub const FORCE_NOT_WRITING: &str = "-f only makes sense when writing a replacement";
pub const RAW_WITHOUT_EDIT: &str = "--raw only makes sense with --edit";
pub const DELETE_NEEDS_ARG: &str = "-d needs at least one argument";
pub const BAD_NUMBER_OF_ARGS: &str = "bad number of arguments";
pub const EDIT_NEEDS_ONE: &str = "-e needs exactly one argument";
pub const GRAFT_NEEDS_ARG: &str = "-g needs at least one argument";
pub const CONVERT_TAKES_NO_ARG: &str = "--convert-graft-file takes no argument";
pub const ONE_PATTERN: &str = "only one pattern can be given with -l";
pub const EDITING_FAILED: &str = "editing object file failed";
pub const MALFORMED: &str = "refusing to create malformed object";
pub const MKTREE_FAILED: &str = "mktree reported failure";
pub const SIGNATURE_REMOVED: &str = "the signature will be removed in the replacement commit!";

pub fn invalid_format(format: &str) -> String {
    format!("invalid replace format '{format}'\nvalid formats are 'short', 'medium' and 'long'")
}

pub fn failed_to_resolve(name: &str) -> String {
    format!("failed to resolve '{name}' as a valid ref")
}

pub fn not_found(hex: &str) -> String {
    format!("replace ref '{hex}' not found")
}

pub fn deleted(hex: &str) -> String {
    format!("Deleted replace ref '{hex}'")
}

pub fn already_exists(refname: &str) -> String {
    format!("replace ref '{refname}' already exists")
}

pub fn type_mismatch(
    object: &str,
    object_type: &str,
    replacement: &str,
    replacement_type: &str,
) -> String {
    format!(
        "Objects must be of the same type.\n\
         '{object}' points to a replaced object of type '{object_type}'\n\
         while '{replacement}' points to a replacement object of type '{replacement_type}'."
    )
}

pub fn not_a_valid_object_name(name: &str) -> String {
    format!("not a valid object name: '{name}'")
}

pub fn not_a_commit(hex: &str, kind: &str) -> String {
    format!("object {hex} is a {kind}, not a commit")
}

pub fn could_not_parse(name: &str) -> String {
    format!("could not parse {name}")
}

pub fn could_not_parse_as_commit(name: &str) -> String {
    format!("could not parse {name} as a commit")
}

pub fn has_signature(hex: &str) -> String {
    format!("the original commit '{hex}' has a gpg signature")
}

pub fn mergetag_discarded(commit: &str, tag: &str) -> String {
    format!(
        "original commit '{commit}' contains mergetag '{tag}' that is discarded; use --edit instead of --graft"
    )
}

pub fn same_commit(hex: &str) -> String {
    format!("new commit is the same as the old one: '{hex}'")
}

pub fn graft_unnecessary(hex: &str) -> String {
    format!("graft for '{hex}' unnecessary")
}

pub fn same_object(hex: &str) -> String {
    format!("new object is the same as the old one: '{hex}'")
}

pub fn fails_fsck(finding: &str) -> String {
    format!("object fails fsck: {finding}")
}

pub fn could_not_convert(lines: &str) -> String {
    format!("could not convert the following graft(s):\n{lines}")
}

pub fn unable_to_read(hex: &str) -> String {
    format!("unable to read {hex}")
}
