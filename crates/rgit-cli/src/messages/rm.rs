// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git rm` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git rm [-f | --force] [-n] [-r] [--cached] [--ignore-unmatch]\n       [--quiet] [--pathspec-from-file=<file> [--pathspec-file-nul]]\n       [--] [<pathspec>...]",
];

pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_QUIET: &str = "do not list removed files";
pub const HELP_CACHED: &str = "only remove from the index";
pub const HELP_FORCE: &str = "override the up-to-date check";
pub const HELP_R: &str = "allow recursive removal";
pub const HELP_IGNORE_UNMATCH: &str = "exit with a zero status even if nothing matched";

pub const NO_PATHSPEC: &str = "No pathspec was given. Which files should I remove?";

#[must_use]
pub fn not_recursive(path: &str) -> String {
    format!("not removing '{path}' recursively without -r")
}

#[must_use]
pub fn rm(path: &str) -> String {
    format!("rm '{path}'")
}

/// Kinds of the up-to-date check failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    Both,
    Staged,
    Local,
}

#[must_use]
pub fn problem(kind: Problem, paths: &[String], cached: bool) -> String {
    let many = paths.len() > 1;
    let head = match (kind, many) {
        (Problem::Both, false) => {
            "the following file has staged content different from both the\nfile and the HEAD:"
        }
        (Problem::Both, true) => {
            "the following files have staged content different from both the\nfile and the HEAD:"
        }
        (Problem::Staged, false) => "the following file has changes staged in the index:",
        (Problem::Staged, true) => "the following files have changes staged in the index:",
        (Problem::Local, false) => "the following file has local modifications:",
        (Problem::Local, true) => "the following files have local modifications:",
    };
    let hint = if kind == Problem::Both || cached {
        "(use -f to force removal)"
    } else {
        "(use --cached to keep the file, or -f to force removal)"
    };
    let mut s = head.to_string();
    for p in paths {
        s.push_str("\n    ");
        s.push_str(p);
    }
    s.push('\n');
    s.push_str(hint);
    s
}
