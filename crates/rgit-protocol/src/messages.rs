// SPDX-License-Identifier: MIT
//! User-visible texts of the protocol layer, kept in one place (see
//! CLEANROOM.md: short functional strings reproduced for compatibility, as
//! observed from git 2.56.0). Texts without a `fatal: ` prefix get it from
//! the caller.

/// A pkt-line length that is not four hex digits; `raw` are the four bytes.
pub fn bad_length_character(raw: &str) -> String {
    format!("protocol error: bad line length character: {raw}")
}

/// A pkt-line length of 3, or a payload that does not fit the read buffer.
pub fn bad_length(len: usize) -> String {
    format!("protocol error: bad line length {len}")
}

/// End of input in the middle of, or where the protocol expects, a packet.
pub const HUNG_UP: &str = "the remote end hung up unexpectedly";

/// An `ERR <msg>` packet.
pub fn remote_error(msg: &str) -> String {
    format!("remote error: {msg}")
}

/// End of input while demultiplexing a sideband stream. git prints it with
/// the program name as prefix (`fetch-pack: ...`).
pub const SIDEBAND_DISCONNECT: &str = "unexpected disconnect while reading sideband packet";

/// An empty packet on a sideband stream (prefixed like
/// [`SIDEBAND_DISCONNECT`]).
pub const MISSING_BAND: &str = "protocol error: missing sideband designator";

/// A sideband packet for a channel other than 1, 2 or 3 (prefixed like
/// [`SIDEBAND_DISCONNECT`]).
pub fn bad_band(band: u8) -> String {
    format!("protocol error: bad band #{band}")
}

/// Prefix of every line of remote progress and of channel 3 messages.
pub const REMOTE_PREFIX: &[u8] = b"remote: ";

/// Erase-to-end-of-line sequence written before each progress line when
/// stderr is a terminal that understands it.
pub const ANSI_CLEAR_LINE: &[u8] = b"\x1b[K";

/// Padding written after a non-empty progress line otherwise, so that a
/// shorter line overwriting a longer one after `\r` hides its tail.
pub const SPACES_SUFFIX: &[u8] = b"        ";

/// Protocol error for a packet the caller did not expect; `what` is the
/// packet as it appears in a trace.
pub fn unexpected(what: &str) -> String {
    format!("protocol error: unexpected '{what}'")
}

/// Protocol v2 `fetch`: the first response section is not the expected
/// one (`acknowledgments` while negotiating, `packfile` after `done`).
pub fn expected_section(expected: &str, received: &str) -> String {
    format!("expected '{expected}', received '{received}'")
}

/// Protocol v2 `fetch`: a line in the `acknowledgments` section that is not
/// `ACK <oid>`, `NAK` or `ready`.
pub fn unexpected_ack_line(line: &str) -> String {
    format!("unexpected acknowledgment line: '{line}'")
}

/// Protocol v2 `fetch`: `ready` was not followed by a delimiter packet.
pub const PACK_AFTER_READY: &str = "expected packfile to be sent after 'ready'";

/// Protocol v2 `fetch`: more sections after an `acknowledgments` section
/// without `ready`.
pub const NO_SECTIONS_WITHOUT_READY: &str =
    "expected no other sections to be sent after no 'ready'";

/// Protocol v2 `fetch`: a bad line in the `shallow-info` section.
pub fn bad_shallow_line(line: &str) -> String {
    format!("expected shallow/unshallow, got {line}")
}

/// Protocol v2 `ls-refs`: a line that does not parse.
pub fn invalid_ls_refs_response(line: &str) -> String {
    format!("invalid ls-refs response: {line}")
}

/// The local repository and the server use different hash algorithms.
pub fn mismatched_algorithms(client: &str, server: &str) -> String {
    format!("mismatched algorithms: client {client}; server {server}")
}

/// Shallow or deepen arguments for a server without the `shallow` feature
/// of `fetch`.
pub const SHALLOW_NOT_SUPPORTED: &str = "Server does not support shallow requests";

/// Warning (printed by the caller with `warning: `) when a filter was asked
/// for but the server does not advertise the `filter` feature of `fetch`;
/// the filter is then not sent.
pub const FILTER_NOT_RECOGNIZED: &str = "filtering not recognized by server, ignoring";

// The following texts are rgit's own wording: git reports these situations
// through internal checks whose texts are not observable from its behaviour.

/// Attempt to send more than 65516 bytes in one pkt-line.
pub fn data_too_long(len: usize) -> String {
    format!("protocol error: packet data too long ({len} bytes)")
}

/// A sideband channel 3 message; it has already been printed as
/// `remote: <msg>` when this error is returned.
pub fn sideband_fatal(msg: &str) -> String {
    format!("remote error: {msg}")
}

/// A v2 capability advertisement that does not start with `version 2`.
pub const EXPECTED_VERSION_2: &str = "protocol error: expected 'version 2'";

/// A server that answered with protocol v0 or v1; only v2 is implemented.
pub fn not_v2(version: u8) -> String {
    format!(
        "protocol error: server answered with protocol version {version}; \
         rgit only speaks version 2 so far"
    )
}

/// The server advertised an `object-format` rgit does not know.
pub fn unknown_object_format(name: &str) -> String {
    format!("protocol error: unknown object format '{name}'")
}

/// The server does not advertise a v2 command.
pub fn command_not_supported(command: &str) -> String {
    format!("protocol error: server does not support the '{command}' command")
}

/// A line in the `wanted-refs` section that does not parse.
pub fn bad_wanted_ref(line: &str) -> String {
    format!("protocol error: bad wanted-ref line '{line}'")
}

/// A request line given by the caller that cannot be sent (it contains LF
/// or does not fit into a pkt-line).
pub fn bad_request_line(line: &str) -> String {
    format!("protocol error: invalid request line '{line}'")
}
