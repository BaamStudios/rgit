// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git name-rev` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git name-rev [<options>] <commit>...",
    "git name-rev [<options>] --all",
    "git name-rev [<options>] --annotate-stdin",
];

pub const HELP_NAME_ONLY: &str = "print only ref-based names (no object names)";
pub const HELP_TAGS: &str = "only use tags to name the commits";
pub const HELP_REFS: &str = "only use refs matching <pattern>";
pub const HELP_EXCLUDE: &str = "ignore refs matching <pattern>";
pub const HELP_ALL: &str = "list all commits reachable from all refs";
pub const HELP_STDIN: &str = "deprecated: use --annotate-stdin instead";
pub const HELP_ANNOTATE_STDIN: &str = "annotate text from stdin";
pub const HELP_UNDEFINED: &str = "allow to print `undefined` names (default)";
pub const HELP_ALWAYS: &str = "show abbreviated commit object as fallback";
pub const HELP_PEEL_TAG: &str = "dereference tags in the input (internal use)";

pub const LIST_OR_ALL: &str = "Specify either a list, or --all, not both!";
pub const STDIN_DEPRECATED: &str = "--stdin is deprecated. Please use --annotate-stdin instead, which is functionally equivalent.\nThis option will be removed in a future release.";
pub const UNDEFINED: &str = "undefined";

pub fn cannot_describe(hex: &str) -> String {
    format!("cannot describe '{hex}'")
}

pub fn could_not_get_sha1(arg: &str) -> String {
    format!("Could not get sha1 for {arg}. Skipping.")
}

pub fn bad_object(hex: &str) -> String {
    format!("bad object {hex}")
}
