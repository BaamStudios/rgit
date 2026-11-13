// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git clone` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git clone [<options>] [--] <repo> [<dir>]"];

pub const HELP_VERBOSE: &str = "be more verbose";
pub const HELP_QUIET: &str = "be more quiet";
pub const HELP_PROGRESS: &str = "force progress reporting";
pub const HELP_REJECT_SHALLOW: &str = "don't clone shallow repository";
pub const HELP_NO_CHECKOUT: &str = "don't create a checkout";
pub const HELP_BARE: &str = "create a bare repository";
pub const HELP_MIRROR: &str = "create a mirror repository (implies --bare)";
pub const HELP_LOCAL: &str = "to clone from a local repository";
pub const HELP_NO_HARDLINKS: &str = "don't use local hardlinks, always copy";
pub const HELP_SHARED: &str = "setup as shared repository";
pub const HELP_RECURSE_SUBMODULES: &str = "initialize submodules in the clone";
pub const HELP_JOBS: &str = "number of submodules cloned in parallel";
pub const HELP_TEMPLATE: &str = "directory from which templates will be used";
pub const HELP_REFERENCE: &str = "reference repository";
pub const HELP_DISSOCIATE: &str = "use --reference only while cloning";
pub const HELP_ORIGIN: &str = "use <name> instead of 'origin' to track upstream";
pub const HELP_BRANCH: &str = "checkout <branch> instead of the remote's HEAD";
pub const HELP_REVISION: &str = "clone single revision <rev> and check out";
pub const HELP_UPLOAD_PACK: &str = "path to git-upload-pack on the remote";
pub const HELP_DEPTH: &str = "create a shallow clone of that depth";
pub const HELP_SHALLOW_SINCE: &str = "create a shallow clone since a specific time";
pub const HELP_SHALLOW_EXCLUDE: &str = "deepen history of shallow clone, excluding ref";
pub const HELP_SINGLE_BRANCH: &str = "clone only one branch, HEAD or --branch";
pub const HELP_TAGS: &str = "clone tags, and make later fetches not to follow them";
pub const HELP_SHALLOW_SUBMODULES: &str = "any cloned submodules will be shallow";
pub const HELP_SEPARATE_GIT_DIR: &str = "separate git dir from working tree";
pub const HELP_REF_FORMAT: &str = "specify the reference format to use";
pub const HELP_CONFIG: &str = "set config inside the new repository";
pub const HELP_SERVER_OPTION: &str = "option to transmit";
pub const HELP_IPV4: &str = "use IPv4 addresses only";
pub const HELP_IPV6: &str = "use IPv6 addresses only";
pub const HELP_FILTER: &str = "object filtering";
pub const HELP_ALSO_FILTER_SUBMODULES: &str = "apply partial clone filters to submodules";
pub const HELP_REMOTE_SUBMODULES: &str =
    "any cloned submodules will use their remote-tracking branch";
pub const HELP_SPARSE: &str = "initialize sparse-checkout file to include only files at root";
pub const HELP_BUNDLE_URI: &str =
    "a URI for downloading bundles before fetching from origin remote";

pub const NO_REPOSITORY: &str = "You must specify a repository to clone.";
pub const TOO_MANY_ARGUMENTS: &str = "Too many arguments.";

pub fn cloning_into(dir: &str) -> String {
    format!("Cloning into '{dir}'...")
}
pub fn cloning_into_bare(dir: &str) -> String {
    format!("Cloning into bare repository '{dir}'...")
}
pub fn not_a_commit(name: &str, oid: &str) -> String {
    format!("{name} {oid} is not a commit!")
}
pub const DONE: &str = "done.";
pub const EMPTY_REPOSITORY: &str = "You appear to have cloned an empty repository.";

pub fn repository_does_not_exist(repo: &str) -> String {
    format!("repository '{repo}' does not exist")
}
pub fn destination_exists(dir: &str) -> String {
    format!("destination path '{dir}' already exists and is not an empty directory.")
}
pub fn cannot_create_leading(dir: &str) -> String {
    format!("could not create leading directories of '{dir}'")
}
pub fn cannot_create_work_tree(dir: &str) -> String {
    format!("could not create work tree dir '{dir}'")
}
pub fn invalid_remote_name(name: &str) -> String {
    format!("'{name}' is not a valid remote name")
}
pub fn depth_not_positive(depth: &str) -> String {
    format!("depth {depth} is not a positive number")
}
pub const DEPTH_IGNORED_LOCAL: &str = "--depth is ignored in local clones; use file:// instead.";
pub fn remote_branch_not_found(branch: &str, remote: &str) -> String {
    format!("Remote branch {branch} not found in upstream {remote}")
}
pub const REMOTE_HEAD_NONEXISTENT: &str =
    "remote HEAD refers to nonexistent ref, unable to checkout";
pub fn reflog_clone(url: &str) -> String {
    format!("clone: from {url}")
}
pub fn invalid_config_pair(arg: &str) -> String {
    format!("error: invalid config pair '{arg}'")
}

/// The advice printed when the clone ends on a detached HEAD.
pub fn detached_head_advice(name: &str) -> String {
    format!(
        "Note: switching to '{name}'.\n\n{}",
        concat!(
            "You are in 'detached HEAD' state. You can look around, make experimental\n",
            "changes and commit them, and you can discard any commits you make in this\n",
            "state without impacting any branches by switching back to a branch.\n",
            "\n",
            "If you want to create a new branch to retain commits you create, you may\n",
            "do so (now or later) by using -c with the switch command. Example:\n",
            "\n",
            "  git switch -c <new-branch-name>\n",
            "\n",
            "Or undo this operation with:\n",
            "\n",
            "  git switch -\n",
            "\n",
            "Turn off this advice by setting config variable advice.detachedHead to false\n",
            "\n",
        )
    )
}

/// rgit-only: an option that is parsed but not implemented yet.
pub fn unsupported(option: &str) -> String {
    format!("rgit: clone {option} is not supported yet")
}
pub const REMOTE_DID_NOT_SEND_ALL: &str = "remote did not send all necessary objects";
pub const UNABLE_TO_CHECKOUT: &str = "unable to checkout working tree";
/// After `warning: `; ends with an empty line.
pub const CHECKOUT_FAILED: &str = "Clone succeeded, but checkout failed.\n\
You can inspect what was checked out with 'git status'\n\
and retry with 'git restore --source=HEAD :/'\n\n";
pub fn unable_to_read_tree(oid: &str) -> String {
    format!("unable to read tree ({oid})")
}
