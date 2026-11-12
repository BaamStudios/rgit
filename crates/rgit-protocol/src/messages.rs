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
