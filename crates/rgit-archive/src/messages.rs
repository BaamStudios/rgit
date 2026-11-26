// SPDX-License-Identifier: MIT
//! User-visible messages of the archive code (CLEANROOM.md: reproduced
//! verbatim where scripts depend on them, collected here for review).

/// Channel 3 message of `upload-archive` when the archiver failed.
pub const ARCHIVER_DIED: &str = "git upload-archive: archiver died with error";

/// The archiver got a packet that is not an argument.
pub const ARGUMENT_EXPECTED: &str = "'argument' token or flush expected";

/// The client got neither `ACK` nor `NACK`.
pub const PROTOCOL_ERROR: &str = "git archive: protocol error";

/// The `ACK` was not followed by a flush packet.
pub const EXPECTED_FLUSH: &str = "git archive: expected a flush";

pub fn too_many_options(max: usize) -> String {
    format!("Too many options (>{max})")
}

pub fn nack(reason: &str) -> String {
    format!("git archive: NACK {reason}")
}

pub fn sent_error(msg: &str) -> String {
    format!("sent error to the client: {msg}")
}
