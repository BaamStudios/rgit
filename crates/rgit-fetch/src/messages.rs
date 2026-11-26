// SPDX-License-Identifier: MIT
//! User-visible messages of `git fetch`, reproduced verbatim from the
//! output of git 2.56 because scripts and tests compare them. Messages are
//! without the `fatal: ` / `error: ` prefix unless noted.

use std::path::Path;

/// `fatal:` for `tag` at the end of the command-line refspecs.
pub const NEED_TAG_NAME: &str = "you need to specify a tag name";

/// `hint:` before [`SERVER_OPTIONS_NEED_V2`].
pub const SERVER_OPTIONS_HINT: &str = "see protocol.version in 'git help config' for more details";

/// `fatal:` for `--server-option` with a protocol v0/v1 server.
pub const SERVER_OPTIONS_NEED_V2: &str = "server options require protocol version 2 or later";

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

/// `warning:` for an importing remote helper without `refspec`
/// capability.
pub const HELPER_NEEDS_REFSPEC: &str = "this remote helper should implement refspec capability";

/// `fatal:` when the private ref of an imported ref is missing.
pub fn could_not_read_ref(name: &str) -> String {
    format!("could not read ref {name}")
}

/// `fatal:` for a `protocol.version` other than 0, 1 or 2.
pub fn unknown_protocol_version(value: &str) -> String {
    format!("unknown value for config 'protocol.version': {value}")
}

/// `fatal:` for a `GIT_TEST_PROTOCOL_VERSION` other than 0, 1 or 2.
pub fn unknown_test_protocol_version(value: &str) -> String {
    format!("unknown value for GIT_TEST_PROTOCOL_VERSION: {value}")
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

/// A ref left out because its history ends in new shallow commits
/// (printed after `warning: `).
pub fn rejected_shallow_root(name: &str) -> String {
    format!("rejected {name} because shallow roots are not allowed to be updated")
}

/// A fetch would store a tag (or another non-commit) in `refs/heads/`.
pub fn non_commit_to_branch(oid: &str, refname: &str) -> String {
    format!("trying to write non-commit object {oid} to branch '{refname}'")
}

/// `followRemoteHEAD=warn`: the remote's `HEAD` is not ours.
pub fn remote_head_differs(remote: &str, theirs: &str, ours: &str) -> String {
    format!("'HEAD' at '{remote}' is '{theirs}', but we have '{ours}' locally.")
}

/// `followRemoteHEAD=warn` with a detached `refs/remotes/<name>/HEAD`.
pub fn remote_head_detached(remote: &str, theirs: &str, oid: &str) -> String {
    format!(
        "'HEAD' at '{remote}' is '{theirs}', but we have a detached HEAD pointing to '{oid}' locally."
    )
}

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

/// `fetch.negotiationAlgorithm` with an unknown value.
pub fn unknown_negotiation_algorithm(value: &str) -> String {
    format!("unknown fetch negotiation algorithm '{value}'")
}

/// Protocol v0/v1: an object that is no advertised ref tip.
pub fn unadvertised_object(hex: &str) -> String {
    format!("Server does not allow request for unadvertised object {hex}")
}

/// `fetch --negotiate-only` against a server without `wait-for-done`.
pub const NO_WAIT_FOR_DONE: &str = "server does not support wait-for-done";

/// git: a received object fails `fetch.fsckObjects`.
pub const FSCK_ERROR: &str = "fsck error in packed object";
/// git: the last line of a fetch whose pack fails its checks.
pub const INDEX_PACK_FAILED: &str = "index-pack failed";
/// git: the last line of a clone whose pack fails its checks.
pub const INVALID_INDEX_PACK_OUTPUT: &str = "fetch-pack: invalid index-pack output";
/// git: a `.gitmodules` blob in another pack than its tree fails the
/// checks (found once every pack is in).
pub const FSCK_FAILED: &str = "fsck failed";
/// git: a tree names a non-blob as `.gitmodules`.
pub const GITMODULES_BLOB: &str = "non-blob found at .gitmodules";

/// git: the pack of a `packfile-uris` URI has another checksum than the
/// server said.
pub fn pack_hash_mismatch(uri: &str, hash: &str) -> String {
    format!("fetch-pack: pack downloaded from {uri} does not match expected hash {hash}")
}

/// git (`http-fetch`): a `packfile-uris` URI cannot be downloaded, with
/// `GIT_TRACE_REDACT` on.
pub fn failed_to_get_url(scheme: &str, host: &str) -> String {
    format!(
        "failed to get '{scheme}' url from '{host}' (full URL redacted due to GIT_TRACE_REDACT setting)"
    )
}

/// git (`http-fetch`): the same with `GIT_TRACE_REDACT=0`.
pub fn unable_to_get_pack_file(uri: &str) -> String {
    format!("unable to get pack file '{uri}'")
}

/// git: the last line after a failed `packfile-uris` download.
pub const NO_HTTP_FETCH_OUTPUT: &str =
    "fetch-pack: expected pack or keep then TAB at start of http-fetch output";

/// git: a `fetch.fsck.<msg-id>` that names no message id.
pub fn skipping_unknown_msg_id(id: &str) -> String {
    format!("Skipping unknown msg id '{id}'")
}

/// git: `fetch.fsck.skipList` names a file that cannot be read.
pub fn could_not_open_skiplist(path: &str) -> String {
    format!("could not open object name list: {path}")
}

/// git: a configuration entry without a value where one is needed.
pub fn missing_config_value(name: &str) -> String {
    format!("missing value for '{name}'")
}

/// git: follows [`missing_config_value`].
pub fn unable_to_parse_config(name: &str) -> String {
    format!("unable to parse '{name}' from command-line config")
}

/// git: a finding of `fetch.fsckObjects`.
pub fn fsck_finding(id: &rgit_hash::ObjectId, finding: &dyn std::fmt::Display) -> String {
    format!("object {id}: {finding}")
}

/// The server does not support `filter`.
pub const FILTER_NOT_RECOGNIZED: &str = "filtering not recognized by server, ignoring";

/// `fatal:` when a bundle cannot be used as fetch source (after its own
/// `error:` lines).
pub const REMOTE_TRANSPORT_ERROR: &str = "remote transport reported error";

/// `error:` before the list of missing prerequisites.
pub const LACKS_PREREQUISITES: &str = "Repository lacks these prerequisite commits:";

/// `error:` for prerequisites whose history is incomplete.
pub const NOT_CONNECTED: &str = "some prerequisite commits exist in the object store, \
but are not connected to the repository's history";

/// `warning:` when a URI could not be downloaded.
pub fn failed_to_download(uri: &str) -> String {
    format!("failed to download bundle from URI '{uri}'")
}

/// `warning:` when the bundle URI given to clone gave nothing.
pub fn failed_to_fetch(uri: &str) -> String {
    format!("failed to fetch objects from bundle URI '{uri}'")
}

/// `warning:` for a file that is neither bundle nor list.
pub fn not_bundle_or_list(uri: &str) -> String {
    format!("file at URI '{uri}' is not a bundle or bundle list")
}

/// `error:` for a URI with spaces or control characters.
pub fn uri_malformed(uri: &str) -> String {
    format!("bundle-uri: URI is malformed: {uri}")
}

/// `error:` for a download target with control characters.
pub fn filename_malformed(path: &str) -> String {
    format!("bundle-uri: filename is malformed: {path}")
}

/// `warning:` for a `promisor.acceptFromServerUrl` value with a bad
/// remote name.
pub fn invalid_promisor_remote_name(name: &str, value: &str) -> String {
    format!(
        "invalid remote name '{name}' before '=' sign in '{value}' from promisor.acceptFromServerUrl config"
    )
}

/// `warning:` for a `promisor.acceptFromServerUrl` value that is no URL.
pub fn invalid_promisor_url_pattern(pattern: &str, value: &str) -> String {
    format!("invalid url pattern '{pattern}' in '{value}' from promisor.acceptFromServerUrl config")
}

/// A remote created for an accepted promisor remote.
pub fn auto_creating_promisor(name: &str, url: &str) -> String {
    format!("Auto-creating promisor remote '{name}' for URL '{url}'")
}

/// A field of an accepted promisor remote stored (`what`: filter, token).
pub fn storing_field(what: &str, remote: &str, old: &str, new: &str) -> String {
    format!("Storing new {what} from server for remote '{remote}'.\n    '{old}' -> '{new}'")
}

/// `warning:` for an advertised filter that does not parse.
pub fn invalid_stored_filter(filter: &str, remote: &str, why: &str) -> String {
    format!("invalid filter '{filter}' for remote '{remote}' will not be stored: {why}")
}

/// `warning:` for an advertised token with control characters.
pub fn invalid_stored_token(token: &str, remote: &str) -> String {
    format!("invalid token '{token}' for remote '{remote}' will not be stored")
}
