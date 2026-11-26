// SPDX-License-Identifier: MIT
//! Usage, option help and messages of `git subtree` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).
//! They were taken from the documentation (contrib/subtree/git-subtree.adoc),
//! from the behaviour of the reference `git subtree` and from the
//! conformance test t7900.

pub const USAGE: &[&str] = &[
    "git subtree add   --prefix=<prefix> [-S[=<key-id>]] <commit>",
    "git subtree add   --prefix=<prefix> [-S[=<key-id>]] <repository> <ref>",
    "git subtree merge --prefix=<prefix> [-S[=<key-id>]] <commit>",
    "git subtree split --prefix=<prefix> [-S[=<key-id>]] [<commit>]",
    "git subtree pull  --prefix=<prefix> [-S[=<key-id>]] <repository> <ref>",
    "git subtree push  --prefix=<prefix> [-S[=<key-id>]] <repository> <refspec>",
];

pub const H_HELP: &str = "show the help";
pub const H_QUIET: &str = "quiet";
pub const H_DEBUG: &str = "show debug messages";
pub const H_PREFIX: &str = "the name of the subdir to split out";
pub const GROUP_SPLIT: &str = "options for 'split' (also: 'push')";
pub const H_ANNOTATE: &str = "add a prefix to commit message of new commits";
pub const H_BRANCH: &str = "create a new branch from the split subtree";
pub const H_IGNORE_JOINS: &str = "ignore prior --rejoin commits";
pub const H_ONTO: &str = "try connecting new tree to an existing one";
pub const H_REJOIN: &str = "merge the new branch back into HEAD";
pub const GROUP_MERGE: &str =
    "options for 'add' and 'merge' (also: 'pull', 'split --rejoin', and 'push --rejoin')";
pub const H_SQUASH: &str = "merge subtree changes as a single commit";
pub const H_MESSAGE: &str = "use the given message as the commit message for the merge commit";
pub const H_GPG_SIGN: &str =
    "GPG-sign commits. The keyid argument is optional and defaults to the committer identity";
pub const ARGH_ANY: &str = "...";
pub const ARGH_KEY_ID: &str = "key-id";

pub const NOT_TOPLEVEL: &str =
    "You need to run this command from the toplevel of the working tree.";
pub const NO_PREFIX: &str = "you must provide the --prefix option.";
pub const NOT_CLEAN: &str = "working tree has modifications.  Cannot add.";
pub const PULL_ARGS: &str = "you must provide <repository> <ref>";
pub const PUSH_ARGS: &str = "you must provide <repository> <refspec>";
pub const NO_NEW_REVISIONS: &str = "No new revisions were found";

pub fn unknown_command(cmd: &str) -> String {
    format!("unknown command '{cmd}'")
}

pub fn wrong_flag(flag: &str, cmd: &str) -> String {
    format!("the '{flag}' flag does not make sense with 'git subtree {cmd}'.")
}

pub fn dir_missing(dir: &str) -> String {
    format!("'{dir}' does not exist; use 'git subtree add'")
}

pub fn prefix_exists(dir: &str) -> String {
    format!("prefix '{dir}' already exists.")
}

pub fn not_a_commit(rev: &str) -> String {
    format!("'{rev}' does not refer to a commit")
}

pub fn not_a_ref(r: &str) -> String {
    format!("'{r}' does not look like a ref")
}

pub fn add_params(args: &str) -> String {
    format!("parameters were '{args}'\nProvide either a commit or a repository and commit.")
}

pub fn merge_args(args: &str) -> String {
    format!("you must provide exactly one revision, and optionally a repository. Got: '{args}'")
}

pub fn split_args(args: &str) -> String {
    format!("you must provide exactly one revision, and optionally a repository.  Got: '{args}'")
}

pub fn never_added(dir: &str) -> String {
    format!("can't squash-merge: '{dir}' was never added.")
}

pub fn already_at(rev: &str) -> String {
    format!("Subtree is already at commit {rev}.")
}

pub fn bad_split_hash(hash: &str, commit: &str) -> String {
    format!(
        "could not rev-parse split hash {hash} from commit {commit}\n\
         hint: hash might be a tag, try fetching it from the subtree repository:\n\
         hint:    git fetch <subtree-repository> {hash}"
    )
}

pub fn cache_exists(rev: &str) -> String {
    format!("cache for {rev} already exists!")
}

pub fn not_ancestor(branch: &str, rev: &str) -> String {
    format!("branch '{branch}' is not an ancestor of commit '{rev}'.")
}

pub fn added_dir(dir: &str) -> String {
    format!("Added dir '{dir}'")
}

pub fn branch_done(updated: bool, branch: &str) -> String {
    let action = if updated { "Updated" } else { "Created" };
    format!("{action} branch '{branch}'")
}

pub fn echo_fetch(repo: &str, refname: &str) -> String {
    format!("git fetch {repo} {refname}")
}

pub fn echo_push(repo: &str, refspec: &str) -> String {
    format!("git push using:  {repo} {refspec}")
}

/// Subject of the merge commit `add` writes.
pub fn add_subject(dir: &str, rev: &str) -> String {
    format!("Add '{dir}/' from commit '{rev}'")
}

/// Subject of the merge commit `add --squash` writes.
pub fn add_squashed_subject(squash: &str, dir: &str) -> String {
    format!("Merge commit '{squash}' as '{dir}'")
}

/// Subject of the merge commit `split --rejoin` writes.
pub fn rejoin_subject(dir: &str, rev: &str) -> String {
    format!("Split '{dir}/' into commit '{rev}'")
}

pub fn squash_content_subject(dir: &str, short: &str) -> String {
    format!("Squashed '{dir}/' content from commit {short}")
}

pub fn squash_changes_subject(dir: &str, old: &str, new: &str) -> String {
    format!("Squashed '{dir}/' changes from {old}..{new}")
}

pub const TRAILER_DIR: &str = "git-subtree-dir:";
pub const TRAILER_MAINLINE: &str = "git-subtree-mainline:";
pub const TRAILER_SPLIT: &str = "git-subtree-split:";
