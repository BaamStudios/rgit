// SPDX-License-Identifier: MIT
//! Messages of `git push`, as printed by the reference git 2.56.0.

pub const USAGE: &[&str] = &["git push [<options>] [<repository> [<refspec>...]]"];

pub const H_VERBOSE: &str = "be more verbose";
pub const H_QUIET: &str = "be more quiet";
pub const H_REPO: &str = "repository";
pub const H_ALL: &str = "push all branches";
pub const H_MIRROR: &str = "mirror all refs";
pub const H_DELETE: &str = "delete refs";
pub const H_TAGS: &str = "push tags (can't be used with --all or --branches or --mirror)";
pub const H_DRY_RUN: &str = "dry run";
pub const H_PORCELAIN: &str = "machine-readable output";
pub const H_FORCE: &str = "force updates";
pub const H_FORCE_WITH_LEASE: &str = "require old value of ref to be at this value";
pub const H_FORCE_IF_INCLUDES: &str = "require remote updates to be integrated locally";
pub const H_RECURSE_SUBMODULES: &str = "control recursive pushing of submodules";
pub const H_THIN: &str = "use thin pack";
pub const H_RECEIVE_PACK: &str = "receive pack program";
pub const H_SET_UPSTREAM: &str = "set upstream for git pull/status";
pub const H_PROGRESS: &str = "force progress reporting";
pub const H_PRUNE: &str = "prune locally removed refs";
pub const H_NO_VERIFY: &str = "bypass pre-push hook";
pub const H_VERIFY: &str = "opposite of --no-verify";
pub const H_FOLLOW_TAGS: &str = "push missing but relevant tags";
pub const H_SIGNED: &str = "GPG sign the push";
pub const H_ATOMIC: &str = "request atomic transaction on remote side";
pub const H_PUSH_OPTION: &str = "option to transmit";
pub const H_IPV4: &str = "use IPv4 addresses only";
pub const H_IPV6: &str = "use IPv6 addresses only";

/// rgit: an option that is recognised but not implemented yet.
pub fn not_supported(opt: &str) -> String {
    format!("rgit push: {opt} is not supported yet")
}

/// rgit: a transport that cannot push yet.
pub fn transport_not_supported(url: &str) -> String {
    format!("rgit push: pushing to '{url}' is not supported yet")
}

/// rgit: hooks are not run yet (decision on #65: warn, never skip
/// silently).
pub const PRE_PUSH_SKIPPED: &str =
    "rgit does not run hooks yet; the pre-push hook was not run (use --no-verify to silence this)";

pub const DELETE_WITHOUT_REFS: &str = "--delete doesn't make sense without any refs";
pub const ALL_WITH_REFSPECS: &str = "--all can't be combined with refspecs";
pub const TAGS_WITH_ALL: &str = "options '--tags' and '--all/--branches' cannot be used together";
pub const EVERYTHING_UP_TO_DATE: &str = "Everything up-to-date";
pub const NO_PUSH_OPTIONS: &str = "the receiving end does not support push options";
pub const NO_ATOMIC: &str = "the receiving end does not support --atomic push";
pub const COULD_NOT_READ: &str = "Could not read from remote repository.\n\n\
Please make sure you have the correct access rights\n\
and the repository exists.";
pub const REFLOG_MESSAGE: &str = "update by push";

pub const NO_DESTINATION: &str = "No configured push destination.\n\
Either specify the URL from the command-line or configure a remote repository using\n\
\n\
    \x20   git remote add <name> <url>\n\
\n\
and then push using the remote name\n\
\n\
    \x20   git push <name>\n\
\n\
To push to multiple remotes at once, configure a remote group using\n\
\n\
    \x20   git config remotes.<groupname> \"<remote1> <remote2>\"\n\
\n\
and then push using the group name\n\
\n\
    \x20   git push <groupname>\n";

pub const PUSH_DEFAULT_NOTHING: &str =
    "You didn't specify any refspecs to push, and push.default is \"nothing\".";

pub fn detached(remote: &str) -> String {
    format!(
        "You are not currently on a branch.\n\
         To push the history leading to the current (detached HEAD)\n\
         state now, use\n\
         \n    git push {remote} HEAD:<name-of-remote-branch>\n"
    )
}

pub fn no_upstream(branch: &str, remote: &str) -> String {
    format!(
        "The current branch {branch} has no upstream branch.\n\
         To push the current branch and set the remote as upstream, use\n\
         \n    git push --set-upstream {remote} {branch}\n\n\
         To have this happen automatically for branches without a tracking\n\
         upstream, see 'push.autoSetupRemote' in 'git help config'.\n"
    )
}

pub fn upstream_mismatch(remote: &str, upstream_short: &str) -> String {
    format!(
        "The upstream branch of your current branch does not match\n\
         the name of your current branch.  To push to the upstream branch\n\
         on the remote, use\n\
         \n    git push {remote} HEAD:{upstream_short}\n\n\
         To push to the branch of the same name on the remote, use\n\
         \n    git push {remote} HEAD\n\n\
         To choose either option permanently, see push.default in 'git help config'.\n\n\
         To avoid automatically configuring an upstream branch when its name\n\
         won't match the local branch, see option 'simple' of branch.autoSetupMerge\n\
         in 'git help config'.\n"
    )
}

pub fn not_upstream_remote(remote: &str, branch: &str) -> String {
    format!(
        "You are pushing to remote '{remote}', which is not the upstream of\n\
         your current branch '{branch}', without telling me what to push\n\
         to update which remote branch."
    )
}

pub fn multiple_upstreams(branch: &str) -> String {
    format!("The current branch {branch} has multiple upstream branches, refusing to push.")
}

pub fn invalid_refspec(spec: &str) -> String {
    format!("invalid refspec '{spec}'")
}

pub fn src_no_match(src: &str) -> String {
    format!("src refspec {src} does not match any")
}

pub fn src_ambiguous(src: &str) -> String {
    format!("src refspec {src} matches more than one")
}

pub fn dst_ambiguous(dst: &str) -> String {
    format!("dst refspec {dst} matches more than one")
}

pub fn dst_multiple_src(dst: &str) -> String {
    format!("dst ref {dst} receives from more than one src")
}

pub fn unable_to_delete(name: &str) -> String {
    format!("unable to delete '{name}': remote ref does not exist")
}

pub fn unqualified_dst(dst: &str, src: &str) -> String {
    format!(
        "The destination you provided is not a full refname (i.e.,\n\
         starting with \"refs/\"). We tried to guess what you meant by:\n\
         \n\
         - Looking for a ref that matches '{dst}' on the remote side.\n\
         - Checking if the <src> being pushed ('{src}')\n  \
         is a ref in \"refs/{{heads,tags}}/\". If so we add a corresponding\n  \
         refs/{{heads,tags}}/ prefix on the remote side.\n\
         \n\
         Neither worked, so we gave up. You must fully qualify the ref."
    )
}

/// Advice after [`unqualified_dst`], by the type of the source object.
pub fn unqualified_dst_hint(kind: &str, src: &str, dst: &str) -> Option<String> {
    let (what, prefix) = match kind {
        "commit" => ("create a new branch", "refs/heads/"),
        "tag" => ("create a new tag", "refs/tags/"),
        "tree" => ("tag a new tree", "refs/tags/"),
        "blob" => ("tag a new blob", "refs/tags/"),
        _ => return None,
    };
    Some(format!(
        "The <src> part of the refspec is a {kind} object.\n\
         Did you mean to {what} by pushing to\n\
         '{src}:{prefix}{dst}'?"
    ))
}

pub fn failed_to_push(url: &str) -> String {
    format!("failed to push some refs to '{url}'")
}

pub fn atomic_failed(name: &str, status: u32) -> String {
    format!("atomic push failed for ref {name}. status: {status}")
}

pub fn pushing_to(url: &str) -> String {
    format!("Pushing to {url}")
}

pub fn updating_tracking(name: &str) -> String {
    format!("updating local tracking ref '{name}'")
}

pub fn upstream_set(branch: &str, remote: &str, dst_short: &str) -> String {
    format!("branch '{branch}' set up to track '{remote}/{dst_short}'.")
}

pub fn would_set_upstream(branch: &str, dst_short: &str, remote: &str) -> String {
    format!("Would set upstream of '{branch}' to '{dst_short}' of '{remote}'")
}

pub const HINT_NON_FF_CURRENT: &str = "Updates were rejected because the tip of your current branch is behind\n\
its remote counterpart. If you want to integrate the remote changes,\n\
use 'git pull' before pushing again.\n\
See the 'Note about fast-forwards' in 'git push --help' for details.";

pub const HINT_NON_FF_MATCHING: &str = "Updates were rejected because a pushed branch tip is behind its remote\n\
counterpart. If you want to integrate the remote changes, use 'git pull'\n\
before pushing again.\n\
See the 'Note about fast-forwards' in 'git push --help' for details.";

pub const HINT_ALREADY_EXISTS: &str =
    "Updates were rejected because the tag already exists in the remote.";

pub const HINT_FETCH_FIRST: &str = "Updates were rejected because the remote contains work that you do not\n\
have locally. This is usually caused by another repository pushing to\n\
the same ref. If you want to integrate the remote changes, use\n\
'git pull' before pushing again.\n\
See the 'Note about fast-forwards' in 'git push --help' for details.";

pub const HINT_NEEDS_FORCE: &str = "You cannot update a remote ref that points at a non-commit object,\n\
or update a remote ref to make it point at a non-commit object,\n\
without using the '--force' option.";
