// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git mv` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here; they
//! were taken from the reference git's output).

pub const USAGE: &[&str] = &[
    "git mv [-v] [-f] [-n] [-k] <source> <destination>",
    "git mv [-v] [-f] [-n] [-k] <source>... <destination-directory>",
];

pub const HELP_VERBOSE: &str = "be verbose";
pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_FORCE: &str = "force move/rename even if target exists";
pub const HELP_K: &str = "skip move/rename errors";
pub const HELP_SPARSE: &str = "allow updating entries outside of the sparse-checkout cone";

pub const BAD_SOURCE: &str = "bad source";
pub const INTO_ITSELF: &str = "can not move directory into itself";
pub const DIR_EMPTY: &str = "source directory is empty";
pub const DIR_DEST_EXISTS: &str = "destination already exists";
pub const NOT_TRACKED: &str = "not under version control";
pub const CONFLICTED: &str = "conflicted";
pub const DEST_EXISTS: &str = "destination exists";
pub const CANNOT_OVERWRITE: &str = "Cannot overwrite";
pub const DEST_DIR_MISSING: &str = "destination directory does not exist";
pub const DEST_NOT_DIR: &str = "destination is not a directory";
pub const DEST_BEYOND_SYMLINK: &str = "destination is beyond a symbolic link";
pub const MULTIPLE_SOURCES: &str = "multiple sources for the same target";

/// A failed check of one move: `<what>, source=<src>, destination=<dst>`.
#[must_use]
pub fn bad_move(what: &str, src: &str, dst: &str) -> String {
    format!("{what}, source={src}, destination={dst}")
}

#[must_use]
pub fn not_a_directory(dest: &str) -> String {
    format!("destination '{dest}' is not a directory")
}

#[must_use]
pub fn outside_repository(path: &str, root: &str) -> String {
    format!("'{path}' is outside repository at '{root}'")
}

#[must_use]
pub fn both_and_parent(path: &str, parent: &str) -> String {
    format!("cannot move both '{path}' and its parent directory '{parent}'")
}

#[must_use]
pub fn checking(src: &str, dst: &str) -> String {
    format!("Checking rename of '{src}' to '{dst}'")
}

#[must_use]
pub fn renaming(src: &str, dst: &str) -> String {
    format!("Renaming {src} to {dst}")
}

#[must_use]
pub fn overwriting(dst: &str) -> String {
    format!("overwriting '{dst}'")
}

#[must_use]
pub fn rename_failed(src: &str, dst: &str, err: &str) -> String {
    format!("renaming '{src}' to '{dst}' failed: {err}")
}

#[must_use]
pub fn invalid_path(path: &str) -> String {
    format!("invalid path '{path}'")
}
