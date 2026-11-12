// SPDX-License-Identifier: MIT
//! User-visible messages of this crate. Where scripts and the conformance
//! tests depend on the wording it is reproduced verbatim (see CLEANROOM.md);
//! everything is collected here so the set can be reviewed in one place.

use std::fmt::Display;

/// Discovery reached a ceiling directory or the root.
pub(crate) const NOT_A_REPO_PARENTS: &str =
    "not a git repository (or any of the parent directories): .git";

/// Discovery stopped at a file system boundary; `mount` is the directory
/// it did not enter.
pub(crate) fn not_a_repo_mount(mount: impl Display) -> String {
    format!(
        "not a git repository (or any parent up to mount point {mount})\n\
         Stopping at filesystem boundary (GIT_DISCOVERY_ACROSS_FILESYSTEM not set)."
    )
}

/// `GIT_DIR` / `--git-dir` does not name a repository.
pub(crate) fn not_a_repo_named(dir: impl Display) -> String {
    format!("not a git repository: '{dir}'")
}

pub(crate) fn invalid_gitfile(path: impl Display) -> String {
    format!("invalid gitfile format: {path}")
}

pub(crate) fn no_path_in_gitfile(path: impl Display) -> String {
    format!("no path in gitfile: {path}")
}

pub(crate) fn gitfile_not_repo(path: impl Display) -> String {
    format!("gitfile does not point to a valid repository: {path}")
}

pub(crate) fn not_regular_file(path: impl Display) -> String {
    format!("not a regular file: '{path}'")
}

pub(crate) fn error_reading(path: impl Display) -> String {
    format!("error reading {path}")
}

pub(crate) fn dubious_ownership(path: impl Display) -> String {
    format!(
        "detected dubious ownership in repository at '{path}'\n\
         To add an exception for this directory, call:\n\
         \n\
         \tgit config --global --add safe.directory {path}"
    )
}

pub(crate) fn safe_directory_not_absolute(value: impl Display) -> String {
    format!("safe.directory '{value}' not absolute")
}

pub(crate) fn bare_not_allowed(path: impl Display) -> String {
    format!("cannot use bare repository '{path}' (safe.bareRepository is 'explicit')")
}

pub(crate) fn unable_to_parse(key: &str, source: impl Display) -> String {
    format!("unable to parse '{key}' from {source}")
}

pub(crate) fn invalid_path(path: impl Display, reason: impl Display) -> String {
    format!("Invalid path '{path}': {reason}")
}

pub(crate) fn cannot_chdir(path: impl Display, reason: impl Display) -> String {
    format!("cannot chdir to '{path}': {reason}")
}

pub(crate) const EMPTY_PATH: &str = "The empty string is not a valid path";

pub(crate) fn repo_version_too_new(version: i64) -> String {
    format!("Expected git repo version <= 1, found {version}")
}

pub(crate) fn unknown_extensions(names: &[String]) -> String {
    list(
        if names.len() == 1 {
            "unknown repository extension found:"
        } else {
            "unknown repository extensions found:"
        },
        names,
    )
}

pub(crate) fn v1_only_extensions(names: &[String]) -> String {
    list(
        if names.len() == 1 {
            "repo version is 0, but v1-only extension found:"
        } else {
            "repo version is 0, but v1-only extensions found:"
        },
        names,
    )
}

fn list(head: &str, names: &[String]) -> String {
    let mut s = head.to_string();
    for n in names {
        s.push_str("\n\t");
        s.push_str(n);
    }
    s
}

pub(crate) fn invalid_extension_value(name: impl Display, value: impl Display) -> String {
    format!("invalid value for '{name}': '{value}'")
}

/// rgit wording: git aborts with an internal BUG() here instead.
pub(crate) fn same_compat_format() -> &'static str {
    "extensions.compatObjectFormat must differ from extensions.objectFormat"
}

/// rgit wording (git has no such message): the repository uses a known
/// extension rgit cannot handle yet.
pub(crate) fn unsupported(what: impl Display) -> String {
    format!("this repository uses {what}, which rgit does not support yet")
}

pub(crate) const BARE_AND_WORKTREE: &str = "core.bare and core.worktree do not make sense";
pub(crate) const INVALID_WORKTREE_CONFIG: &str = "unable to set up work tree using invalid config";
pub(crate) const MUST_RUN_IN_WORK_TREE: &str = "this operation must be run in a work tree";

// `git init` (see `init`).

pub(crate) fn templates_not_found(path: impl Display) -> String {
    format!("templates not found in {path}")
}

pub(crate) fn not_copying_templates(path: impl Display, reason: impl Display) -> String {
    format!("not copying templates from '{path}': {reason}")
}

/// rgit wording: copying one template file or directory failed.
pub(crate) fn cannot_copy_template(
    from: impl Display,
    to: impl Display,
    reason: impl Display,
) -> String {
    format!("cannot copy '{from}' to '{to}': {reason}")
}

pub(crate) fn reinit_ignored_branch(name: impl Display) -> String {
    format!("re-init: ignored --initial-branch={name}")
}

pub(crate) fn invalid_initial_branch(name: impl Display) -> String {
    format!("invalid initial branch name: '{name}'")
}

pub(crate) fn invalid_default_branch(name: impl Display) -> String {
    format!("invalid branch name: init.defaultBranch = {name}")
}

pub(crate) fn unknown_hash(name: impl Display) -> String {
    format!("unknown hash algorithm '{name}'")
}

pub(crate) fn unknown_ref_format(name: impl Display) -> String {
    format!("unknown ref storage format '{name}'")
}

pub(crate) const REINIT_DIFFERENT_HASH: &str =
    "attempt to reinitialize repository with different hash";

pub(crate) const REINIT_DIFFERENT_REF_FORMAT: &str =
    "attempt to reinitialize repository with different reference storage format";

pub(crate) const SEPARATE_GIT_DIR_BARE: &str =
    "--separate-git-dir incompatible with bare repository";

pub(crate) fn unable_to_move(from: impl Display, to: impl Display, reason: impl Display) -> String {
    format!("unable to move {from} to {to}: {reason}")
}

pub(crate) fn cannot_mkdir(path: impl Display, reason: impl Display) -> String {
    format!("cannot mkdir {path}: {reason}")
}

/// rgit wording: a file of the new repository could not be written.
pub(crate) fn cannot_write(path: impl Display, reason: impl Display) -> String {
    format!("unable to write '{path}': {reason}")
}

/// rgit wording (git creates such repositories): the new repository would
/// need a format rgit cannot handle yet.
pub(crate) fn init_unsupported(what: impl Display) -> String {
    format!("cannot create a repository with {what}, which rgit does not support yet")
}
