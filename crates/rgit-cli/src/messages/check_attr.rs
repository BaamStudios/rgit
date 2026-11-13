// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git check-attr` and of
//! reading attributes in commands (CLEANROOM.md: user-visible strings are
//! reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git check-attr [--source <tree-ish>] [-a | --all | <attr>...] [--] <pathname>...",
    "git check-attr --stdin [-z] [--source <tree-ish>] [-a | --all | <attr>...]",
];

pub const HELP_ALL: &str = "report all attributes set on file";
pub const HELP_CACHED: &str = "use .gitattributes only from the index";
pub const HELP_STDIN: &str = "read file names from stdin";
pub const HELP_Z: &str = "terminate input and output records by a NUL character";
pub const HELP_SOURCE: &str = "which tree-ish to check attributes at";
pub const ARGH_SOURCE: &str = "tree-ish";

pub const ATTRS_AND_ALL: &str = "Attributes and --all both specified";
pub const NO_ATTRIBUTE: &str = "No attribute specified";
pub const FILES_WITH_STDIN: &str = "Can't specify files with --stdin";
pub const NO_FILE: &str = "No file specified";
pub const BADLY_QUOTED: &str = "line is badly quoted";
pub const BAD_ATTR_SOURCE: &str = "bad --attr-source or GIT_ATTR_SOURCE";

pub fn invalid_attribute(name: &str) -> String {
    format!("{name}: not a valid attribute name")
}

pub fn invalid_source(source: &str) -> String {
    format!("{source}: not a valid tree-ish source")
}

pub fn outside_repository(path: &str, root: &str) -> String {
    format!("'{path}' is outside repository at '{root}'")
}

pub fn unable_to_stat(path: &str, err: &str) -> String {
    format!("unable to stat '{path}': {err}")
}
