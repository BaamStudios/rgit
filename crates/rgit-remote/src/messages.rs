// SPDX-License-Identifier: MIT
//! User-visible messages of this crate, verbatim where scripts and the
//! conformance tests depend on them (see CLEANROOM.md). Every string a git
//! user may see is built here so the set can be reviewed in one place.

use std::fmt::Display;

use bstr::BStr;
use rgit_config::Origin;

pub(crate) fn invalid_refspec(spec: &BStr) -> String {
    format!("invalid refspec '{spec}'")
}

pub(crate) fn couldnt_find_remote_ref(name: &BStr) -> String {
    format!("couldn't find remote ref {name}")
}

pub(crate) fn cannot_fetch_both(first: &BStr, second: &BStr, dst: &BStr) -> String {
    format!("Cannot fetch both {first} and {second} to {dst}")
}

pub(crate) fn ignoring_funny_ref(dst: &BStr) -> String {
    format!("* Ignoring funny ref '{dst}' locally")
}

pub(crate) fn protocol_not_supported(proto: &BStr) -> String {
    format!("protocol '{proto}' is not supported")
}

pub(crate) const NO_PATH: &str = "no path specified; see 'git help pull' for valid url syntax";

pub(crate) const NO_DIRECTORY_NAME: &str =
    "No directory name could be guessed.\nPlease specify a directory on the command line";

pub(crate) fn transport_not_allowed(proto: &BStr) -> String {
    format!("transport '{proto}' not allowed")
}

pub(crate) fn unknown_config_value(name: &str, value: &BStr) -> String {
    format!("unknown value for config '{name}': {value}")
}

pub(crate) fn bad_env_bool(value: &BStr, name: &str) -> String {
    format!("bad boolean environment value '{value}' for '{name}'")
}

pub(crate) fn missing_value(name: &BStr) -> String {
    format!("missing value for '{name}'")
}

/// The fatal line after `missing value for '<name>'` for a file or blob.
pub(crate) fn bad_config_variable(name: &BStr, origin: &Origin, line: usize) -> String {
    format!(
        "bad config variable '{name}' in {} '{}' at line {line}",
        origin.kind(),
        origin.name()
    )
}

/// The fatal line after an error from command-line configuration.
pub(crate) fn unable_to_parse_cmdline(name: &BStr) -> String {
    format!("unable to parse '{name}' from command-line config")
}

/// The warning printed when a remote is read from `$GIT_DIR/remotes/` or
/// `$GIT_DIR/branches/`.
pub(crate) fn legacy_remote_warning(kind: &str, name: &BStr) -> String {
    format!(
        "reading remote from \"{kind}/{name}\", which is nominated for removal.\n\
         \n\
         If you still use the \"remotes/\" directory it is recommended to\n\
         migrate to config-based remotes:\n\
         \n\
         \tgit remote rename {name} {name}\n\
         \n\
         If you cannot, please let us know why you still need to use it by\n\
         sending an e-mail to <git@vger.kernel.org>."
    )
}

pub(crate) const HEAD_NOT_A_BRANCH: &str = "HEAD does not point to a branch";

pub(crate) fn no_upstream(branch: &BStr) -> String {
    format!("no upstream configured for branch '{branch}'")
}

pub(crate) fn upstream_not_stored(merge: &BStr) -> String {
    format!("upstream branch '{merge}' not stored as a remote-tracking branch")
}

pub(crate) const PUSH_NOTHING: &str = "push has no destination (push.default is 'nothing')";

pub(crate) fn cannot_resolve_push(mode: impl Display) -> String {
    format!("cannot resolve '{mode}' push to a single destination")
}

pub(crate) fn push_refspecs_exclude(remote: &BStr, branch: &BStr) -> String {
    format!("push refspecs for '{remote}' do not include '{branch}'")
}

pub(crate) fn push_dst_untracked(dst: &BStr, remote: &BStr) -> String {
    format!("push destination '{dst}' on remote '{remote}' has no local tracking branch")
}

pub(crate) fn malformed_push_default(value: &BStr) -> [String; 2] {
    [
        format!("malformed value for push.default: {value}"),
        "must be one of nothing, matching, simple, upstream or current".to_string(),
    ]
}
