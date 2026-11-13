// SPDX-License-Identifier: MIT
//! User-visible messages of `git fetch`, reproduced verbatim from the
//! output of git 2.56 because scripts and tests compare them. Messages are
//! without the `fatal: ` / `error: ` prefix unless noted.

use std::path::Path;

/// `fatal:` for `tag` at the end of the command-line refspecs.
pub const NEED_TAG_NAME: &str = "you need to specify a tag name";

/// `fatal:` when a connection could not be read from the start.
pub const COULD_NOT_READ_REMOTE: &str = "Could not read from remote repository.\n\n\
Please make sure you have the correct access rights\nand the repository exists.";

/// `fatal:` for a destination that is the checked-out branch.
pub fn refusing_to_fetch_into(refname: &str, worktree: &Path) -> String {
    format!(
        "refusing to fetch into branch '{refname}' checked out at '{}'",
        worktree.display()
    )
}

/// `fatal:` when no transport handles the URL's scheme.
pub fn no_remote_helper(scheme: &str) -> String {
    format!("unable to find remote helper for '{scheme}'")
}

/// `fatal:` when there is no URL to fetch from.
pub const NO_URL: &str = "No remote repository specified.  Please, specify either a URL or a\n\
remote name from which new revisions should be fetched.";

/// `error:` when the received objects do not complete the new tips.
pub fn did_not_send_all(url: &str) -> String {
    format!("{url} did not send all necessary objects")
}

/// `fatal:` for an object that is still missing after the transfer.
pub fn bad_object(id: &str) -> String {
    format!("bad object {id}")
}

/// `fatal:` when `FETCH_HEAD` cannot be written.
pub fn cannot_open_fetch_head(path: &Path, err: &str) -> String {
    format!("cannot open '{}': {err}", path.display())
}

/// Reason shown after a rejected non-fast-forward update.
pub const NON_FAST_FORWARD: &str = "non-fast-forward";
/// Reason shown after a rejected tag update.
pub const WOULD_CLOBBER_TAG: &str = "would clobber existing tag";
/// Note after a forced update.
pub const FORCED_UPDATE: &str = "forced update";
/// Reason shown when the local ref could not be written.
pub const UNABLE_TO_UPDATE: &str = "unable to update local ref";

/// Summary column texts.
pub const NEW_BRANCH: &str = "[new branch]";
/// Summary column texts.
pub const NEW_TAG: &str = "[new tag]";
/// Summary column texts.
pub const NEW_REF: &str = "[new ref]";
/// Summary column texts.
pub const TAG_UPDATE: &str = "[tag update]";
/// Summary column texts.
pub const UP_TO_DATE: &str = "[up to date]";
/// Summary column texts.
pub const REJECTED: &str = "[rejected]";
/// Summary column texts.
pub const DELETED: &str = "[deleted]";
/// The remote side of a pruned ref.
pub const NONE: &str = "(none)";

/// The first line of the summary.
pub fn from_url(url: &str) -> String {
    format!("From {url}")
}

/// Reflog messages (after `<action>: `).
pub const LOG_STORING_HEAD: &str = "storing head";
/// Reflog messages (after `<action>: `).
pub const LOG_STORING_TAG: &str = "storing tag";
/// Reflog messages (after `<action>: `).
pub const LOG_STORING_REF: &str = "storing ref";
/// Reflog messages (after `<action>: `).
pub const LOG_FAST_FORWARD: &str = "fast-forward";
/// Reflog messages (after `<action>: `).
pub const LOG_FORCED_UPDATE: &str = "forced-update";
/// Reflog messages (after `<action>: `).
pub const LOG_UPDATING_TAG: &str = "updating tag";
