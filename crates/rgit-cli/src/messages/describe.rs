// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git describe` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git describe [--all] [--tags] [--contains] [--abbrev=<n>] [<commit-ish>...]",
    "git describe [--all] [--tags] [--contains] [--abbrev=<n>] --dirty[=<mark>]",
    "git describe <blob>",
];

pub const HELP_CONTAINS: &str = "find the tag that comes after the commit";
pub const HELP_DEBUG: &str = "debug search strategy on stderr";
pub const HELP_ALL: &str = "use any ref";
pub const HELP_TAGS: &str = "use any tag, even unannotated";
pub const HELP_LONG: &str = "always use long format";
pub const HELP_FIRST_PARENT: &str = "only follow first parent";
pub const HELP_ABBREV: &str = "use <n> digits to display object names";
pub const HELP_EXACT_MATCH: &str = "only output exact matches";
pub const HELP_CANDIDATES: &str = "consider <n> most recent tags (default: 10)";
pub const HELP_MATCH: &str = "only consider tags matching <pattern>";
pub const HELP_EXCLUDE: &str = "do not consider tags matching <pattern>";
pub const HELP_ALWAYS: &str = "show abbreviated commit object as fallback";
pub const HELP_DIRTY: &str = "append <mark> on dirty working tree (default: \"-dirty\")";
pub const HELP_BROKEN: &str = "append <mark> on broken working tree (default: \"-broken\")";

pub const NO_NAMES: &str = "No names found, cannot describe anything.";
pub const SEARCHING: &str = "No exact match on refs or tags, searching to describe";

pub fn incompatible(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub fn option_and_commitishes(opt: &str) -> String {
    format!("option '{opt}' and commit-ishes cannot be used together")
}

pub fn not_a_valid_object(name: &str) -> String {
    format!("Not a valid object name {name}")
}

pub fn neither_commit_nor_blob(name: &str) -> String {
    format!("{name} is neither a commit nor blob")
}

pub fn no_exact_match(hex: &str) -> String {
    format!("no tag exactly matches '{hex}'")
}

pub fn no_annotated_tags(hex: &str) -> String {
    format!(
        "No annotated tags can describe '{hex}'.\nHowever, there were unannotated tags: try --tags."
    )
}

pub fn no_tags(hex: &str) -> String {
    format!("No tags can describe '{hex}'.\nTry --always, or create some tags.")
}

pub fn externally_known(path: &str, tag: &str) -> String {
    format!("tag '{path}' is externally known as '{tag}'")
}

pub fn blob_unreachable(hex: &str) -> String {
    format!("blob '{hex}' not reachable from HEAD")
}

pub fn blob_unborn(hex: &str) -> String {
    format!("cannot search for blob '{hex}' on an unborn branch")
}

pub fn debug_describe(arg: &str) -> String {
    format!("describe {arg}")
}

/// One candidate of the `--debug` list.
pub fn debug_candidate(kind: &str, depth: usize, name: &str) -> String {
    format!(" {kind:<11} {depth:>8} {name}")
}

pub fn debug_traversed(n: usize) -> String {
    format!("traversed {n} commits")
}

pub fn debug_gave_up(max: usize, hex: &str) -> String {
    format!("found {max} tags; gave up search at {hex}")
}

pub fn debug_finished(hex: &str) -> String {
    format!("finished search at {hex}")
}

/// The kinds in the `--debug` list, by priority.
pub const KIND_HEAD: &str = "head";
pub const KIND_LIGHTWEIGHT: &str = "lightweight";
pub const KIND_ANNOTATED: &str = "annotated";
