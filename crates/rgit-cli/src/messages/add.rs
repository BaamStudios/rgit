// SPDX-License-Identifier: MIT
//! Messages, usage lines and option help of `git add` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git add [<options>] [--] <pathspec>..."];

pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_VERBOSE: &str = "be verbose";
pub const HELP_FORCE: &str = "allow adding otherwise ignored files";
pub const HELP_UPDATE: &str = "update tracked files";
pub const HELP_ALL: &str = "add changes from all tracked and untracked files";

pub const NOTHING_SPECIFIED: &str = "Nothing specified, nothing added.";
pub const HINT_ADD_DOT: &str = "hint: Maybe you wanted to say 'git add .'?\nhint: Disable this message with \"git config set advice.addEmptyPathspec false\"";
pub const IGNORED_HEADER: &str = "The following paths are ignored by one of your .gitignore files:";
pub const IGNORED_HINT: &str = "hint: Use -f if you really want to add them.\nhint: Disable this message with \"git config set advice.addIgnoredFile false\"";
pub const UPDATE_AND_ALL: &str = "options '-u/--update' and '-A/--all' cannot be used together";

#[must_use]
pub fn add(path: &str) -> String {
    format!("add '{path}'")
}

#[must_use]
pub fn remove(path: &str) -> String {
    format!("remove '{path}'")
}

/// rgit-specific (decision on #107): attributes that would convert content
/// are not applied yet.
#[must_use]
pub fn attributes_not_applied(path: &str) -> String {
    format!("rgit: eol/filter attributes are not applied yet; '{path}' added unconverted")
}
