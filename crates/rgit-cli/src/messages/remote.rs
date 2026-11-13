// SPDX-License-Identifier: MIT
//! Messages of `git remote`, as printed by the reference git.

pub const USAGE: &[&str] = &[
    "git remote [-v | --verbose]",
    "git remote add [-t <branch>] [-m <master>] [-f] [--tags | --no-tags] [--mirror=<fetch|push>] <name> <url>",
    "git remote rename [--[no-]progress] <old> <new>",
    "git remote remove <name>",
    "git remote set-head <name> (-a | --auto | -d | --delete | <branch>)",
    "git remote [-v | --verbose] show [-n] <name>",
    "git remote prune [-n | --dry-run] <name>",
    "git remote [-v | --verbose] update [-p | --prune] [(<group> | <remote>)...]",
    "git remote set-branches [--add] <name> <branch>...",
    "git remote get-url [--push] [--all] <name>",
    "git remote set-url [--push] <name> <newurl> [<oldurl>]",
    "git remote set-url --add <name> <newurl>",
    "git remote set-url --delete <name> <url>",
];
pub const USAGE_ADD: &[&str] = &["git remote add [<options>] <name> <url>"];
pub const USAGE_RENAME: &[&str] = &["git remote rename [--[no-]progress] <old> <new>"];
pub const USAGE_REMOVE: &[&str] = &["git remote remove <name>"];
pub const USAGE_SET_HEAD: &[&str] =
    &["git remote set-head <name> (-a | --auto | -d | --delete | <branch>)"];
pub const USAGE_SET_BRANCHES: &[&str] = &[
    "git remote set-branches <name> <branch>...",
    "git remote set-branches --add <name> <branch>...",
];
pub const USAGE_GET_URL: &[&str] = &["git remote get-url [--push] [--all] <name>"];
pub const USAGE_SET_URL: &[&str] = &[
    "git remote set-url [--push] <name> <newurl> [<oldurl>]",
    "git remote set-url --add <name> <newurl>",
    "git remote set-url --delete <name> <url>",
];
pub const USAGE_SHOW: &[&str] = &["git remote show [<options>] <name>"];
pub const USAGE_PRUNE: &[&str] = &["git remote prune [<options>] <name>"];
pub const USAGE_UPDATE: &[&str] = &["git remote update [<options>] [<group> | <remote>]..."];

pub const HELP_VERBOSE: &str = "be verbose; must be placed before a subcommand";
pub const HELP_FETCH: &str = "fetch the remote branches";
pub const HELP_TAGS: &str = "import all tags and associated objects when fetching\nor do not fetch any tag at all (--no-tags)";
pub const HELP_TRACK: &str = "branch(es) to track";
pub const HELP_MASTER: &str = "master branch";
pub const HELP_MIRROR: &str = "set up remote as a mirror to push to or fetch from";
pub const ARGH_MIRROR: &str = "(push|fetch)";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_AUTO: &str = "set refs/remotes/<name>/HEAD according to remote";
pub const HELP_DELETE_HEAD: &str = "delete refs/remotes/<name>/HEAD";
pub const HELP_ADD_BRANCH: &str = "add branch";
pub const HELP_GET_PUSH: &str = "query push URLs rather than fetch URLs";
pub const HELP_GET_ALL: &str = "return all URLs";
pub const HELP_SET_PUSH: &str = "manipulate push URLs";
pub const HELP_SET_ADD: &str = "add URL";
pub const HELP_SET_DELETE: &str = "delete URLs";
pub const HELP_NO_QUERY: &str = "do not query remotes";
pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_UPDATE_PRUNE: &str = "prune remotes after fetching";

pub const MIRROR_DEPRECATED: &str =
    "--mirror is dangerous and deprecated; please\n\t use --mirror=fetch or --mirror=push instead";
pub const MASTER_WITH_MIRROR: &str = "specifying a master branch makes no sense with --mirror";
pub const TRACK_WITH_PUSH_MIRROR: &str =
    "specifying branches to track makes sense only with fetch mirrors";
pub const NO_REMOTE_SPECIFIED: &str = "no remote specified";
pub const ADD_DELETE: &str = "--add --delete doesn't make sense";
pub const DELETE_ALL_URLS: &str = "Will not delete all non-push URLs";
pub const CANNOT_DETERMINE_HEAD: &str = "Cannot determine remote HEAD";
pub const MULTIPLE_HEADS: &str =
    "Multiple remote HEAD branches. Please choose one explicitly with:";
pub const RENAME_CONFLICT_HINT: &str =
    "The remote you are trying to rename has conflicting references in the
new target refspec. This is most likely caused by you trying to nest
a remote into itself, e.g. by renaming 'parent' into 'parent/child'
or by unnesting a remote, e.g. the other way round.

If that is the case, you can address this by first renaming the
remote to a different name.";
pub const NOTE_ONE_BRANCH: &str =
    "Note: A branch outside the refs/remotes/ hierarchy was not removed;
to delete it, use:";
pub const NOTE_BRANCHES: &str =
    "Note: Some branches outside the refs/remotes/ hierarchy were not removed;
to delete them, use:";

pub fn unknown_subcommand(name: &str) -> String {
    format!("unknown subcommand: `{name}'")
}

pub fn unknown_mirror(arg: &str) -> String {
    format!("unknown --mirror argument: {arg}")
}

pub fn already_exists(name: &str) -> String {
    format!("remote {name} already exists.")
}

pub fn invalid_name(name: &str) -> String {
    format!("'{name}' is not a valid remote name")
}

pub fn subset_of(name: &str, other: &str) -> String {
    format!("remote name '{name}' is a subset of existing remote '{other}'")
}

pub fn superset_of(name: &str, other: &str) -> String {
    format!("remote name '{name}' is a superset of existing remote '{other}'")
}

/// `rename` and `remove`.
pub fn no_such_remote_colon(name: &str) -> String {
    format!("No such remote: '{name}'")
}

/// `get-url`, `set-url`, `set-branches`.
pub fn no_such_remote(name: &str) -> String {
    format!("No such remote '{name}'")
}

pub fn updating(name: &str) -> String {
    format!("Updating {name}")
}

pub fn could_not_fetch_add(name: &str) -> String {
    format!("Could not fetch {name}")
}

pub fn fetching(name: &str) -> String {
    format!("Fetching {name}")
}

pub fn could_not_fetch(name: &str) -> String {
    format!("could not fetch {name}")
}

pub fn no_such_group(name: &str) -> String {
    format!("no such remote or remote group: {name}")
}

pub fn not_updating_refspec(spec: &str) -> String {
    format!(
        "Not updating non-default fetch refspec\n\t{spec}\n\tPlease update the configuration manually if necessary."
    )
}

pub fn more_than_one(key: &str) -> String {
    format!("more than one {key}")
}

pub fn push_default_dangling(scope: &str, origin: &str, name: &str) -> String {
    format!(
        "The {scope} configuration remote.pushDefault in:\n\t{origin}\nnow names the non-existent remote '{name}'"
    )
}

pub fn rename_refs_failed(err: &str) -> String {
    format!("renaming remote references failed: {err}")
}

pub fn renamed_reflog(old: &str, new: &str) -> String {
    format!("remote: renamed {old} to {new}")
}

pub fn branch_delete_hint(name: &str) -> String {
    format!("  git branch -d {name}")
}

pub const SET_HEAD_REFLOG: &str = "remote set-head";

pub fn not_a_valid_ref(name: &str) -> String {
    format!("Not a valid ref: {name}")
}

pub fn could_not_set_up(name: &str) -> String {
    format!("Could not set up {name}")
}

pub fn could_not_delete(name: &str) -> String {
    format!("Could not delete {name}")
}

pub fn set_head_choice(remote: &str, branch: &str) -> String {
    format!("  git remote set-head {remote} {branch}")
}

pub fn head_created(remote: &str, head: &str) -> String {
    format!("'{remote}/HEAD' is now created and points to '{head}'")
}

pub fn head_unchanged(remote: &str, head: &str) -> String {
    format!("'{remote}/HEAD' is unchanged and points to '{head}'")
}

pub fn head_changed(remote: &str, prev: &str, head: &str) -> String {
    format!("'{remote}/HEAD' has changed from '{prev}' and now points to '{head}'")
}

pub fn head_was_detached(remote: &str, oid: &str, head: &str) -> String {
    format!("'{remote}/HEAD' was detached at '{oid}' and now points to '{head}'")
}

pub fn head_was_strange(remote: &str, prev: &str, head: &str) -> String {
    format!(
        "'{remote}/HEAD' used to point to '{prev}' (which is not a remote branch), but now points to '{head}'"
    )
}

pub fn invalid_url_pattern(pattern: &str) -> String {
    format!("Invalid old URL pattern: {pattern}")
}

pub fn no_such_url(url: &str) -> String {
    format!("No such URL found: {url}")
}

pub fn could_not_unset(key: &str) -> String {
    format!("could not unset '{key}'")
}

pub fn could_not_set(key: &str, value: &str) -> String {
    format!("could not set '{key}' to '{value}'")
}

pub fn has_multiple_values(key: &str) -> String {
    format!("{key} has multiple values")
}

// `show`

pub fn show_header(name: &str) -> String {
    format!("* remote {name}")
}

pub fn fetch_url(url: &str) -> String {
    format!("  Fetch URL: {url}")
}

pub fn push_url(url: &str) -> String {
    format!("  Push  URL: {url}")
}

pub const NO_URL: &str = "(no URL)";
pub const HEAD_NOT_QUERIED: &str = "  HEAD branch: (not queried)";
pub const HEAD_UNKNOWN: &str = "  HEAD branch: (unknown)";
pub const HEAD_AMBIGUOUS: &str =
    "  HEAD branch (remote HEAD is ambiguous, may be one of the following):";

pub fn head_branch(name: &str) -> String {
    format!("  HEAD branch: {name}")
}

pub fn remote_branches(count: usize, not_queried: bool) -> String {
    let what = if count == 1 {
        "  Remote branch:"
    } else {
        "  Remote branches:"
    };
    if not_queried {
        format!("{what} (status not queried)")
    } else {
        what.to_owned()
    }
}

pub const STATUS_TRACKED: &str = "tracked";
pub const STATUS_SKIPPED: &str = "skipped";
pub const STATUS_STALE: &str = "stale (use 'git remote prune' to remove)";

pub fn status_new(remote: &str) -> String {
    format!("new (next fetch will store in remotes/{remote})")
}

pub fn pull_branches(count: usize) -> &'static str {
    if count == 1 {
        "  Local branch configured for 'git pull':"
    } else {
        "  Local branches configured for 'git pull':"
    }
}

pub fn invalid_rebase_merge(branch: &str) -> String {
    format!("invalid branch.{branch}.merge; cannot rebase onto > 1 branch")
}

pub const REBASES: &str = "rebases onto remote";
pub const REBASES_INTERACTIVELY: &str = "rebases interactively onto remote";
pub const REBASES_MERGES: &str = "rebases interactively (with merges) onto remote";
pub const MERGES: &str = "merges with remote";
pub const AND_WITH: &str = "   and with remote";

pub const MIRRORED_BY_PUSH: &str = "  Local refs will be mirrored by 'git push'";

pub fn push_refs(count: usize, not_queried: bool) -> String {
    let what = if count == 1 {
        "  Local ref configured for 'git push'"
    } else {
        "  Local refs configured for 'git push'"
    };
    if not_queried {
        format!("{what} (status not queried):")
    } else {
        format!("{what}:")
    }
}

pub const MATCHING: &str = "(matching)";
pub const PUSHES_TO: &str = "pushes to";
pub const FORCES_TO: &str = "forces to";
pub const PUSH_CREATE: &str = "create";
pub const PUSH_DELETE: &str = "delete";
pub const PUSH_UP_TO_DATE: &str = "up to date";
pub const PUSH_FAST_FORWARD: &str = "fast-forwardable";
pub const PUSH_OUT_OF_DATE: &str = "local out of date";

// `prune`

pub fn pruning(name: &str) -> String {
    format!("Pruning {name}")
}

pub fn prune_url(url: &str) -> String {
    format!("URL: {url}")
}

pub fn would_prune(name: &str) -> String {
    format!(" * [would prune] {name}")
}

pub fn pruned(name: &str) -> String {
    format!(" * [pruned] {name}")
}

pub fn dangling_after_prune(symref: &str, target: &str) -> String {
    format!(" {symref} has become dangling after {target} was deleted")
}
