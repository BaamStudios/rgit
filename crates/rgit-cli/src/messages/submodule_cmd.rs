// SPDX-License-Identifier: MIT
//! Messages, usage and option help of the `git submodule` command (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

/// The usage of `git submodule` itself (printed for `-h`, unknown
/// subcommands and misplaced options).
pub const USAGE: &str = "\
usage: git submodule [--quiet] [--cached]
   or: git submodule [--quiet] add [-b <branch>] [-f|--force] [--name <name>] [--reference <repository>] [--] <repository> [<path>]
   or: git submodule [--quiet] status [--cached] [--recursive] [--] [<path>...]
   or: git submodule [--quiet] init [--] [<path>...]
   or: git submodule [--quiet] deinit [-f|--force] (--all| [--] <path>...)
   or: git submodule [--quiet] update [--init [--filter=<filter-spec>]] [--remote] [-N|--no-fetch] [-f|--force] [--checkout|--merge|--rebase] [--[no-]recommend-shallow] [--reference <repository>] [--recursive] [--[no-]single-branch] [--] [<path>...]
   or: git submodule [--quiet] set-branch (--default|--branch <branch>) [--] <path>
   or: git submodule [--quiet] set-url [--] <path> <newurl>
   or: git submodule [--quiet] summary [--cached|--files] [--summary-limit <n>] [commit] [--] [<path>...]
   or: git submodule [--quiet] foreach [--recursive] <command>
   or: git submodule [--quiet] sync [--recursive] [--] [<path>...]
   or: git submodule [--quiet] absorbgitdirs [--] [<path>...]
";

pub const USAGE_ADD: &[&str] = &["git submodule add [<options>] [--] <repository> [<path>]"];
pub const USAGE_STATUS: &[&str] =
    &["git submodule status [--quiet] [--cached] [--recursive] [<path>...]"];
pub const USAGE_INIT: &[&str] = &["git submodule init [<options>] [<path>]"];
pub const USAGE_DEINIT: &[&str] =
    &["git submodule deinit [--quiet] [-f | --force] [--all | [--] [<path>...]]"];
pub const USAGE_UPDATE: &[&str] = &[
    "git submodule [--quiet] update [--init [--filter=<filter-spec>]] [--remote] [-N|--no-fetch] [-f|--force] [--checkout|--merge|--rebase] [--[no-]recommend-shallow] [--reference <repository>] [--recursive] [--[no-]single-branch] [--] [<path>...]",
];
pub const USAGE_SET_BRANCH: &[&str] = &[
    "git submodule set-branch [-q|--quiet] (-d|--default) <path>",
    "git submodule set-branch [-q|--quiet] (-b|--branch) <branch> <path>",
];
pub const USAGE_SET_URL: &[&str] = &["git submodule set-url [--quiet] <path> <newurl>"];
pub const USAGE_SUMMARY: &[&str] = &["git submodule summary [<options>] [<commit>] [--] [<path>]"];
pub const USAGE_FOREACH: &[&str] =
    &["git submodule foreach [--quiet] [--recursive] [--] <command>"];
pub const USAGE_SYNC: &[&str] = &["git submodule sync [--quiet] [--recursive] [<path>]"];
pub const USAGE_ABSORB: &[&str] = &["git submodule absorbgitdirs [<options>] [<path>...]"];
pub const USAGE_HELPER: &[&str] = &["git submodule--helper <command>"];
pub const USAGE_DEFAULT_REMOTE: &[&str] = &["git submodule--helper get-default-remote <path>"];

pub const HELP_ADD_BRANCH: &str = "branch of repository to add as submodule";
pub const HELP_ADD_FORCE: &str = "allow adding an otherwise ignored submodule path";
pub const HELP_ADD_QUIET: &str = "print only error messages";
pub const HELP_PROGRESS: &str = "force cloning progress";
pub const HELP_REFERENCE: &str = "reference repository";
pub const HELP_REF_FORMAT: &str = "specify the reference format to use";
pub const HELP_ADD_DISSOCIATE: &str = "borrow the objects from reference repositories";
pub const HELP_NAME: &str =
    "sets the submodule's name to the given string instead of defaulting to its path";
pub const HELP_ADD_DEPTH: &str = "depth for shallow clones";
pub const HELP_STATUS_QUIET: &str = "suppress submodule status output";
pub const HELP_STATUS_CACHED: &str =
    "use commit stored in the index instead of the one stored in the submodule HEAD";
pub const HELP_RECURSIVE: &str = "recurse into nested submodules";
pub const HELP_INIT_QUIET: &str = "suppress output for initializing a submodule";
pub const HELP_DEINIT_FORCE: &str =
    "remove submodule working trees even if they contain local changes";
pub const HELP_DEINIT_ALL: &str = "unregister all submodules";
pub const HELP_UPDATE_FORCE: &str = "force checkout updates";
pub const HELP_UPDATE_INIT: &str = "initialize uninitialized submodules before update";
pub const HELP_UPDATE_REMOTE: &str = "use SHA-1 of submodule's remote tracking branch";
pub const HELP_UPDATE_RECURSIVE: &str = "traverse submodules recursively";
pub const HELP_NO_FETCH: &str = "don't fetch new objects from the remote site";
pub const HELP_FETCH: &str = "opposite of --no-fetch";
pub const HELP_CHECKOUT: &str = "use the 'checkout' update strategy (default)";
pub const HELP_MERGE: &str = "use the 'merge' update strategy";
pub const HELP_REBASE: &str = "use the 'rebase' update strategy";
pub const HELP_UPDATE_DISSOCIATE: &str = "use --reference only while cloning";
pub const HELP_UPDATE_DEPTH: &str =
    "create a shallow clone truncated to the specified number of revisions";
pub const HELP_JOBS: &str = "parallel jobs";
pub const HELP_RECOMMEND_SHALLOW: &str =
    "whether the initial clone should follow the shallow recommendation";
pub const HELP_UPDATE_QUIET: &str = "don't print cloning progress";
pub const HELP_REQUIRE_INIT: &str = "disallow cloning into non-empty directory, implies --init";
pub const HELP_SINGLE_BRANCH: &str = "clone only one branch, HEAD or --branch";
pub const HELP_FILTER: &str = "object filtering";
pub const HELP_SET_DEFAULT: &str = "set the default tracking branch to master";
pub const HELP_SET_BRANCH: &str = "set the default tracking branch";
pub const HELP_SET_URL_QUIET: &str = "suppress output for setting url of a submodule";
pub const HELP_SUMMARY_CACHED: &str =
    "use the commit stored in the index instead of the submodule HEAD";
pub const HELP_FILES: &str = "compare the commit in the index with that in the submodule HEAD";
pub const HELP_FOR_STATUS: &str = "skip submodules with 'ignore_config' value set to 'all'";
pub const HELP_SUMMARY_LIMIT: &str = "limit the summary size";
pub const HELP_FOREACH_QUIET: &str = "suppress output of entering each submodule command";
pub const HELP_SYNC_QUIET: &str = "suppress output of synchronizing submodule url";

// Errors.

pub fn no_mapping(path: &str) -> String {
    format!("no submodule mapping found in .gitmodules for path '{path}'")
}
pub fn no_url_found(path: &str) -> String {
    format!("No url found for submodule path '{path}' in .gitmodules")
}
pub fn not_initialized(path: &str) -> String {
    format!("Submodule path '{path}' not initialized")
}
pub const MAYBE_INIT: &str = "Maybe you want to use 'update --init'?";
pub fn no_remote_url(remote: &str) -> String {
    format!(
        "could not look up configuration 'remote.{remote}.url'. Assuming this repository is its own authoritative upstream."
    )
}
pub fn registered(name: &str, url: &str, path: &str) -> String {
    format!("Submodule '{name}' ({url}) registered for path '{path}'")
}
pub fn unregistered(name: &str, url: &str, path: &str) -> String {
    format!("Submodule '{name}' ({url}) unregistered for path '{path}'")
}
pub fn cleared_directory(path: &str) -> String {
    format!("Cleared directory '{path}'")
}
pub fn could_not_remove_work_tree(path: &str) -> String {
    format!("could not remove submodule work tree '{path}'")
}
pub fn could_not_create_empty_dir(path: &str) -> String {
    format!("could not create empty submodule directory {path}")
}
pub const USE_ALL: &str = "Use '--all' if you really want to deinitialize all submodules";
pub const ALL_WITH_PATHSPEC: &str = "pathspec and --all are incompatible";
pub fn local_modifications(path: &str) -> String {
    format!("Submodule work tree '{path}' contains local modifications; use '-f' to discard them")
}
pub const RM_LOCAL_MODIFICATIONS: &str = "the following file has local modifications:";
pub const RM_HINT: &str = "(use --cached to keep the file, or -f to force removal)";
pub fn contains_git_dir(path: &str) -> String {
    format!(
        "Submodule work tree '{path}' contains a .git directory. This will be replaced with a .git file by using absorbgitdirs."
    )
}
pub fn synchronizing(path: &str) -> String {
    format!("Synchronizing submodule url for '{path}'")
}
pub fn entering(path: &str) -> String {
    format!("Entering '{path}'")
}
pub fn foreach_failed(path: &str) -> String {
    format!("run_command returned non-zero status for {path}\n.")
}
pub fn foreach_recursion_failed(path: &str) -> String {
    format!(
        "run_command returned non-zero status while recursing in the nested submodules of {path}\n."
    )
}
pub fn skipping_unmerged(path: &str) -> String {
    format!("Skipping unmerged submodule {path}")
}
pub fn skipping(path: &str) -> String {
    format!("Skipping submodule '{path}'")
}
pub fn cannot_clone_without_url(name: &str) -> String {
    format!("cannot clone submodule '{name}' without a URL")
}
pub fn clone_failed(url: &str, path: &str) -> String {
    format!("clone of '{url}' into submodule path '{path}' failed")
}
pub fn checked_out(path: &str, oid: &str) -> String {
    format!("Submodule path '{path}': checked out '{oid}'")
}
pub fn rebased(path: &str, oid: &str) -> String {
    format!("Submodule path '{path}': rebased into '{oid}'")
}
pub fn merged(path: &str, oid: &str) -> String {
    format!("Submodule path '{path}': merged in '{oid}'")
}
pub fn custom_done(path: &str, cmd: &str, oid: &str) -> String {
    format!("Submodule path '{path}': '{cmd} {oid}'")
}
pub fn unable_to_checkout(oid: &str, path: &str) -> String {
    format!("Unable to checkout '{oid}' in submodule path '{path}'")
}
pub fn unable_to_rebase(oid: &str, path: &str) -> String {
    format!("Unable to rebase '{oid}' in submodule path '{path}'")
}
pub fn unable_to_merge(oid: &str, path: &str) -> String {
    format!("Unable to merge '{oid}' in submodule path '{path}'")
}
pub fn custom_failed(cmd: &str, oid: &str, path: &str) -> String {
    format!("Execution of '{cmd} {oid}' failed in submodule path '{path}'")
}
pub fn unable_to_fetch(path: &str) -> String {
    format!("Unable to fetch in submodule path '{path}'")
}
pub fn fetched_but_missing(path: &str, oid: &str) -> String {
    format!(
        "Fetched in submodule path '{path}', but it did not contain {oid}. Direct fetching of that commit failed."
    )
}
pub fn no_current_revision(remote: &str, path: &str) -> String {
    format!("Unable to find current {remote} revision in submodule path '{path}'")
}
pub fn failed_recurse(path: &str) -> String {
    format!("Failed to recurse into submodule path '{path}'")
}
pub fn invalid_update_mode(mode: &str, path: &str) -> String {
    format!("Invalid update mode '{mode}' configured for submodule path '{path}'")
}
pub const RELATIVE_FROM_TOPLEVEL: &str =
    "Relative path can only be used from the toplevel of the working tree";
pub fn invalid_name(name: &str) -> String {
    format!("'{name}' is not a valid submodule name")
}
pub fn name_used(name: &str, path: &str) -> String {
    format!("submodule name '{name}' already used for path '{path}'")
}
pub fn in_index(path: &str) -> String {
    format!("'{path}' already exists in the index")
}
pub fn in_index_not_submodule(path: &str) -> String {
    format!("'{path}' already exists in the index and is not a submodule")
}
pub fn no_commit_checked_out(path: &str) -> String {
    format!("'{path}' does not have a commit checked out")
}
pub const DIFFERENT_HASH: &str = "cannot add a submodule of a different hash algorithm";
pub fn adding_existing(path: &str) -> String {
    format!("Adding existing repo at '{path}' to the index")
}
pub fn not_valid_repo(path: &str) -> String {
    format!("'{path}' already exists and is not a valid git repo")
}
pub fn git_dir_found_locally(name: &str) -> String {
    format!("A git directory for '{name}' is found locally with remote(s):")
}
pub fn reuse_hint(url: &str) -> String {
    format!(
        "If you want to reuse this local git directory instead of cloning again from\n  {url}\nuse the '--force' option. If the local git directory is not the correct repo\nor you are unsure what this means choose another name with the '--name' option."
    )
}
pub fn reactivating(name: &str) -> String {
    format!("Reactivating local git directory for submodule '{name}'")
}
pub fn unable_to_checkout_submodule(path: &str) -> String {
    format!("Unable to checkout submodule '{path}'")
}
pub fn failed_to_add(path: &str) -> String {
    format!("Failed to add submodule '{path}'")
}
pub fn failed_to_register(path: &str) -> String {
    format!("Failed to register submodule '{path}'")
}
pub const STAGE_GITMODULES: &str =
    "please stage your changes to .gitmodules or stash them to proceed";
pub const BRANCH_OR_DEFAULT: &str = "--branch or --default required";
pub const BRANCH_AND_DEFAULT: &str = "options '--branch' and '--default' cannot be used together";
pub fn could_not_lookup_name(path: &str) -> String {
    format!("could not lookup name for submodule '{path}'")
}
pub fn migrating(path: &str, from: &str, to: &str) -> String {
    format!("Migrating git directory of '{path}' from\n'{from}' to\n'{to}'")
}
pub fn could_not_migrate(path: &str) -> String {
    format!("could not migrate git directory from '{path}'")
}
pub fn path_not_submodule(path: &str) -> String {
    format!("could not get a repository handle for submodule '{path}'")
}
pub const CACHED_AND_FILES: &str = "options '--cached' and '--files' cannot be used together";
pub fn summary_does_not_contain(path: &str, oid: &str) -> String {
    format!("  Warn: {path} doesn't contain commit {oid}")
}
pub fn summary_missing_commits(path: &str, from: &str, to: &str) -> String {
    format!("  Warn: {path} doesn't contain commits {from} and {to}")
}
pub const SUMMARY_BLOB: &str = "blob";
pub const SUMMARY_SUBMODULE: &str = "submodule";
pub fn outside_repository(path: &str, root: &str) -> String {
    format!("{path}: '{path}' is outside repository at '{root}'")
}
pub fn invalid_update_value(name: &str) -> String {
    format!("invalid value for 'submodule.{name}.update'")
}
pub fn preparing_tasks(n: i64) -> String {
    format!("run_processes_parallel: preparing to run up to {n} tasks")
}
pub fn retry_scheduled(path: &str) -> String {
    format!("Failed to clone '{path}'. Retry scheduled")
}
pub fn second_failure(path: &str) -> String {
    format!("Failed to clone '{path}' a second time, aborting")
}
pub fn path_is_symlink(path: &str) -> String {
    format!("expected submodule path '{path}' not to be a symbolic link")
}
pub fn leading_symlink(dir: &str, path: &str) -> String {
    format!("expected '{dir}' in submodule path '{path}' not to be a symbolic link")
}
pub fn could_not_lock_config(path: &str) -> String {
    format!("could not lock config file {path}: No such file or directory")
}
pub fn could_not_unset_worktree(name: &str) -> String {
    format!("Could not unset core.worktree setting in submodule '{name}'")
}
