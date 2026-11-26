// SPDX-License-Identifier: MIT
//! User-visible messages of `upload-pack`, word for word as git 2.56
//! prints them (observed from the binary; scripts and tests match on
//! them). Messages marked "rgit" are our own.

use rgit_hash::ObjectId;

/// A request line that is neither a command nor an advertised capability.
pub fn unknown_capability(line: &str) -> String {
    format!("unknown capability '{line}'")
}

/// A request without `command=`.
pub const NO_COMMAND: &str = "no command requested";

/// `command=<name>` for something that is no (enabled) command.
pub fn invalid_command(name: &str) -> String {
    format!("invalid command '{name}'")
}

/// A second `command=` line.
pub fn command_after_command(new: &str, old: &str) -> String {
    format!("command '{new}' requested after already requesting command '{old}'")
}

/// The client's `object-format` is not the repository's.
pub fn mismatched_object_format(server: &str, client: &str) -> String {
    format!("mismatched object format: server {server}; client {client}")
}

/// A capability that needs a value was sent without one.
pub fn capability_requires_argument(name: &str) -> String {
    format!("{name} capability requires an argument")
}

/// An argument line a command does not know.
pub fn unexpected_line(line: &str) -> String {
    format!("unexpected line: '{line}'")
}

/// The arguments of `command` do not end with a flush packet.
pub fn expected_flush_after(command: &str) -> String {
    format!("expected flush after {command} arguments")
}

/// The connection ended inside a request.
pub const HUNG_UP: &str = "the remote end hung up unexpectedly";

/// `want <oid>` for an object the repository does not have: the `ERR`
/// packet.
pub fn not_our_ref_packet(id: &ObjectId) -> String {
    format!("upload-pack: not our ref {id}")
}

/// `want <oid>` for an object the repository does not have: the message
/// on standard error.
pub fn not_our_ref(id: &ObjectId) -> String {
    format!("git upload-pack: not our ref {id}")
}

/// `want` without an object id.
pub fn expected_oid(line: &str) -> String {
    format!("git upload-pack: protocol error, expected to get oid, not '{line}'")
}

/// `have` without an object id.
pub fn expected_object(arg: &str) -> String {
    format!("git upload-pack: expected SHA1 object, got '{arg}'")
}

/// `shallow` without an object id.
pub fn invalid_shallow_line(line: &str) -> String {
    format!("invalid shallow line: {line}")
}

/// `deepen` without a positive number.
pub fn invalid_deepen(line: &str) -> String {
    format!("Invalid deepen: {line}")
}

/// `deepen-since` without a timestamp.
pub fn invalid_deepen_since(line: &str) -> String {
    format!("Invalid deepen-since: {line}")
}

/// `deepen-not` with something that is no reference.
pub fn deepen_not_is_not_a_ref(line: &str) -> String {
    format!("git upload-pack: deepen-not is not a ref: {line}")
}

/// `deepen` together with `deepen-since` or `deepen-not`.
pub const DEEPEN_CONFLICT: &str =
    "git upload-pack: deepen and deepen-since (or deepen-not) cannot be used together";

/// `deepen-since`/`deepen-not` leave no commit.
pub const NO_COMMITS_SELECTED: &str = "no commits selected for shallow requests";

/// `want-ref` for a reference that does not exist (packet and standard
/// error).
pub fn unknown_ref(name: &str) -> String {
    format!("unknown ref {name}")
}

/// A filter that `uploadpackfilter.*` does not allow (packet and standard
/// error).
pub fn filter_not_supported(name: &str) -> String {
    format!("filter '{name}' not supported")
}

/// `tree:<depth>` beyond `uploadpackfilter.tree.maxDepth` (packet and
/// standard error).
pub fn tree_depth_exceeded(max: u64, got: u64) -> String {
    format!("tree filter allows max depth {max}, but got {got}")
}

/// `pack-objects` failed: the first line on standard error (after
/// `error: `).
pub const PACK_OBJECTS_DIED: &str = "git upload-pack: git-pack-objects died with error.";

/// `pack-objects` failed: the message on sideband 3.
pub const ABORTING_PACKET: &str =
    "aborting due to possible repository corruption on the remote side.";

/// `pack-objects` failed: the fatal message on standard error.
pub const ABORTING: &str =
    "git upload-pack: aborting due to possible repository corruption on the remote side.";

/// `object-info`: an unknown argument (sent as `ERR` packet).
pub fn object_info_unexpected_line(line: &str) -> String {
    format!("object-info: unexpected line: '{line}'")
}

/// `object-info`: `oid` without an object id (sent as `ERR` packet).
pub fn object_info_expected_oid(arg: &str) -> String {
    format!("object-info: protocol error, expected to get oid, not '{arg}'")
}

/// rgit: `pack-objects` could not be started.
pub fn cannot_run_pack_objects(err: &std::io::Error) -> String {
    format!("git upload-pack: unable to fork git-pack-objects: {err}")
}

/// Protocol v0: a line of the `want` list that is none of the known
/// requests, or `want` without an object id.
pub fn expected_object_id(line: &str) -> String {
    format!("git upload-pack: protocol error, expected to get object ID, not '{line}'")
}

/// Protocol v0: a line of the negotiation that is neither `have` nor
/// `done`.
pub fn expected_list(line: &str) -> String {
    format!("git upload-pack: expected SHA1 list, got '{line}'")
}

/// Protocol v0: `shallow` names an object that is no commit.
pub fn invalid_shallow_object(id: &ObjectId) -> String {
    format!("invalid shallow object {id}")
}

/// Protocol v0: a `filter` line without the `filter` capability.
pub const FILTER_NOT_NEGOTIATED: &str = "git upload-pack: filtering capability not negotiated";

/// Protocol v0: a second `filter` line.
pub const MULTIPLE_FILTERS: &str = "multiple filter-specs cannot be combined";
