// SPDX-License-Identifier: MIT
//! Messages about upstream tracking of new branches (`branch`, `switch`,
//! `checkout`), as printed by the reference git.

pub const HELP_TRACK: &str = "set branch tracking configuration";
pub const TRACK_ARGH: &str = "(direct|inherit)";
pub const TRACK_EXPECTS: &str = "option `--track' expects \"direct\" or \"inherit\"";
pub const MULTIPLE_REFS_REBASE: &str =
    "cannot inherit upstream tracking configuration of multiple refs when rebasing is requested";

pub fn malformed_auto_setup_rebase() -> String {
    "malformed value for branch.autosetuprebase".to_owned()
}

pub fn refname_ambiguous(name: &str) -> String {
    format!("refname '{name}' is ambiguous.")
}

pub fn ambiguous_object_name(name: &str) -> String {
    format!("ambiguous object name: '{name}'")
}

pub fn not_a_branch(name: &str) -> String {
    format!("cannot set up tracking information; starting point '{name}' is not a branch")
}

pub fn upstream_does_not_exist(name: &str) -> String {
    format!("the requested upstream branch '{name}' does not exist")
}

pub const HINT_SET_UPSTREAM_FAILURE: &str = "hint:
hint: If you are planning on basing your work on an upstream
hint: branch that already exists at the remote, you may need to
hint: run \"git fetch\" to retrieve it.
hint:
hint: If you are planning to push out a new local branch that
hint: will track its remote counterpart, you may want to use
hint: \"git push -u\" to set the upstream config as you push.
hint: Disable this message with \"git config set advice.setUpstreamFailure false\"
";

pub fn inherit_no_remote(name: &str) -> String {
    format!("asked to inherit tracking from '{name}', but no remote is set")
}

pub fn inherit_no_merge(name: &str) -> String {
    format!("asked to inherit tracking from '{name}', but no merge configuration is set")
}

pub fn ambiguous_tracking(full: &str) -> String {
    format!("not tracking: ambiguous information for ref '{full}'")
}

/// The hint after [`ambiguous_tracking`], one line per remote.
pub fn hint_ambiguous_tracking(full: &str, remotes: &[String]) -> String {
    let mut s = format!(
        "hint: There are multiple remotes whose fetch refspecs map to the remote\n\
         hint: tracking ref '{full}':\n"
    );
    for r in remotes {
        s.push_str(&format!("hint:   {r}\n"));
    }
    s.push_str(
        "hint:\n\
         hint: This is typically a configuration error.\n\
         hint:\n\
         hint: To support setting up tracking branches, ensure that\n\
         hint: different remotes' fetch refspecs map into different\n\
         hint: tracking namespaces.\n",
    );
    s
}

pub fn set_up_to_track(branch: &str, upstream: &str, rebase: bool) -> String {
    if rebase {
        format!("branch '{branch}' set up to track '{upstream}' by rebasing.")
    } else {
        format!("branch '{branch}' set up to track '{upstream}'.")
    }
}

/// Several upstreams: a header line followed by one indented line each.
pub fn set_up_to_track_many(branch: &str, upstreams: &[String]) -> String {
    let mut s = format!("branch '{branch}' set up to track:");
    for u in upstreams {
        s.push_str(&format!("\n  {u}"));
    }
    s
}

pub fn own_upstream(name: &str) -> String {
    format!("not setting branch '{name}' as its own upstream")
}

/// `switch`/`checkout <name>` found `<name>` in several remotes.
#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn hint_ambiguous_remote_branch(name: &str, remotes: &[String], command: &str) -> String {
    let mut s = format!("hint: Branch name '{name}' appears in multiple remotes:\n");
    for r in remotes {
        s.push_str(&format!("hint:   {r}\n"));
    }
    s.push_str(&format!(
        "hint: If you meant to check out a remote tracking branch on <remote>,\n\
         hint: you can do so by fully qualifying the name with the --track option:\n\
         hint:\n\
         hint:     git {command} --track <remote>/{name}\n\
         hint:\n\
         hint: If you'd like to always have checkouts of an ambiguous name prefer\n\
         hint: one remote, e.g. the 'origin' remote, consider setting\n\
         hint: checkout.defaultRemote=origin in your config.\n"
    ));
    s
}

#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn matched_multiple_remotes(name: &str, n: usize) -> String {
    format!("'{name}' matched multiple ({n}) remote tracking branches")
}

#[allow(dead_code)] // for switch/checkout (#42, #49)
pub fn missing_branch_name(create_option: &str) -> String {
    format!("missing branch name; try -{create_option}")
}
