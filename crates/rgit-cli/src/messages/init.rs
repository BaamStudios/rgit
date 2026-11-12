// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git init` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &str = "git init [-q | --quiet] [--bare] [--template=<template-directory>]\n         [--separate-git-dir <git-dir>] [--object-format=<format>]\n         [--ref-format=<format>]\n         [-b <branch-name> | --initial-branch=<branch-name>]\n         [--shared[=<permissions>]] [<directory>]";

pub const HELP_TEMPLATE: &str = "directory from which templates will be used";
pub const HELP_BARE: &str = "create a bare repository";
pub const HELP_SHARED: &str =
    "specify that the git repository is to be shared amongst several users";
pub const HELP_QUIET: &str = "be quiet";
pub const HELP_SEPARATE_GIT_DIR: &str = "separate git dir from working tree";
pub const HELP_INITIAL_BRANCH: &str = "override the name of the initial branch";
pub const HELP_OBJECT_FORMAT: &str = "specify the hash algorithm to use";
pub const HELP_REF_FORMAT: &str = "specify the reference format to use";

pub const SEPARATE_AND_BARE: &str =
    "options '--separate-git-dir' and '--bare' cannot be used together";

pub const WORK_TREE_WITHOUT_GIT_DIR: &str = "GIT_WORK_TREE (or --work-tree=<directory>) not allowed without specifying GIT_DIR (or --git-dir=<directory>)";

pub fn cannot_mkdir(dir: &str, reason: &str) -> String {
    format!("cannot mkdir {dir}: {reason}")
}

pub fn cannot_chdir(dir: &str, reason: &str) -> String {
    format!("cannot chdir to {dir}: {reason}")
}

pub fn cannot_access_work_tree(dir: &str, reason: &str) -> String {
    format!("Cannot access work tree '{dir}': {reason}")
}

/// The last line of a successful run.
pub fn done(reinit: bool, shared: bool, git_dir: &str) -> String {
    format!(
        "{}{} Git repository in {git_dir}/",
        if reinit {
            "Reinitialized existing"
        } else {
            "Initialized empty"
        },
        if shared { " shared" } else { "" }
    )
}

/// The `defaultBranchName` advice, one entry per `hint:` line.
pub const DEFAULT_BRANCH_ADVICE: &[&str] = &[
    "Using 'master' as the name for the initial branch. This default branch name",
    "will change to \"main\" in Git 3.0. To configure the initial branch name",
    "to use in all of your new repositories, which will suppress this warning,",
    "call:",
    "",
    "\tgit config --global init.defaultBranch <name>",
    "",
    "Names commonly chosen instead of 'master' are 'main', 'trunk' and",
    "'development'. The just-created branch can be renamed via this command:",
    "",
    "\tgit branch -m <name>",
    "",
    "Disable this message with \"git config set advice.defaultBranchName false\"",
];
