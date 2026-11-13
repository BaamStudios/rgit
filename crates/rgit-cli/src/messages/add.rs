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
/// Advice `addEmptyPathspec`.
pub const HINT_ADD_DOT: &str = "Maybe you wanted to say 'git add .'?";
pub const IGNORED_HEADER: &str = "The following paths are ignored by one of your .gitignore files:";
/// Advice `addIgnoredFile`.
pub const IGNORED_HINT: &str = "Use -f if you really want to add them.";
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

pub const HELP_INTENT_TO_ADD: &str = "record only the fact that the path will be added later";
pub const HELP_IGNORE_ERRORS: &str = "just skip files which cannot be added because of errors";
pub const HELP_IGNORE_MISSING: &str = "check if - even missing - files are ignored in dry run";
pub const HELP_SPARSE: &str = "allow updating entries outside of the sparse-checkout cone";
pub const HELP_CHMOD: &str = "override the executable bit of the listed files";
pub const HELP_PATHSPEC_FROM_FILE: &str = "read pathspec from file";
pub const HELP_PATHSPEC_FILE_NUL: &str =
    "with --pathspec-from-file, pathspec elements are separated with NUL character";

pub const ADDING_FAILED: &str = "adding files failed";
pub const UPDATING_FAILED: &str = "updating files failed";
pub const FROM_FILE_WITH_ARGS: &str =
    "'--pathspec-from-file' and pathspec arguments cannot be used together";

/// `the option '<a>' requires '<b>'`.
#[must_use]
pub fn option_requires(a: &str, b: &str) -> String {
    format!("the option '{a}' requires '{b}'")
}

#[must_use]
pub fn bad_chmod(param: &str) -> String {
    format!("--chmod param '{param}' must be either -x or +x")
}

#[must_use]
pub fn cannot_chmod(flip: char, path: &str) -> String {
    format!("cannot chmod {flip}x '{path}'")
}

#[must_use]
pub fn unable_to_index(path: &str) -> String {
    format!("unable to index file '{path}'")
}

pub const HELP_NO_WARN_EMBEDDED_REPO: &str = "warn when adding an embedded repository";

#[must_use]
pub fn embedded_repo(path: &str) -> String {
    format!("adding embedded git repository: {path}")
}

#[must_use]
pub fn embedded_repo_hint(path: &str) -> String {
    format!(
        "You've added another git repository inside your current repository.
Clones of the outer repository will not contain the contents of
the embedded repository and will not know how to obtain it.
If you meant to add a submodule, use:

\tgit submodule add <url> {path}

If you added this path by mistake, you can remove it from the
index with:

\tgit rm --cached {path}

See \"git help submodule\" for more information."
    )
}

#[must_use]
pub fn no_commit_checked_out(path: &str) -> String {
    format!("'{path}' does not have a commit checked out")
}

#[must_use]
pub fn beyond_symlink(item: &str) -> String {
    format!("pathspec '{item}' is beyond a symbolic link")
}

#[must_use]
pub fn could_not_open(path: &str, err: &str) -> String {
    format!("could not open '{path}' for reading: {err}")
}
