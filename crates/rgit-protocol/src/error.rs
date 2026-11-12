// SPDX-License-Identifier: MIT
//! Error type of the protocol layer.

use std::fmt;
use std::io;

use crate::messages;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors of pkt-line framing, sideband demultiplexing and capability
/// parsing. `Display` gives git's message without the `fatal: ` prefix.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The four length bytes are not hex digits.
    BadLengthCharacter([u8; 4]),
    /// Length 3 (the raw value), or a payload too large to read (the payload
    /// length, i.e. length minus 4).
    BadLength(usize),
    /// End of input inside a packet, or where a packet was required.
    HungUp,
    /// An `ERR` packet; the message without `ERR ` and trailing LF.
    Remote(String),
    /// Attempt to write a packet whose payload exceeds
    /// [`MAX_DATA_LEN`](crate::pktline::MAX_DATA_LEN).
    DataTooLong(usize),
    /// End of input while demultiplexing a sideband stream.
    SidebandDisconnect,
    /// An empty packet on a sideband stream.
    MissingBand,
    /// A sideband packet for an unknown channel.
    BadBand(u8),
    /// A message on sideband channel 3. It has already been written to the
    /// progress output as `remote: <msg>`, so callers should not print it
    /// again.
    SidebandFatal(String),
    /// A packet the caller did not expect, as it would appear in a trace.
    Unexpected(String),
    /// A v2 capability advertisement without `version 2`.
    ExpectedVersion2,
    /// I/O error of the underlying stream.
    Io(#[from] io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadLengthCharacter(raw) => f.write_str(&messages::bad_length_character(
                &String::from_utf8_lossy(raw),
            )),
            Error::BadLength(len) => f.write_str(&messages::bad_length(*len)),
            Error::HungUp => f.write_str(messages::HUNG_UP),
            Error::Remote(msg) => f.write_str(&messages::remote_error(msg)),
            Error::DataTooLong(len) => f.write_str(&messages::data_too_long(*len)),
            Error::SidebandDisconnect => f.write_str(messages::SIDEBAND_DISCONNECT),
            Error::MissingBand => f.write_str(messages::MISSING_BAND),
            Error::BadBand(band) => f.write_str(&messages::bad_band(*band)),
            Error::SidebandFatal(msg) => f.write_str(&messages::sideband_fatal(msg)),
            Error::Unexpected(what) => f.write_str(&messages::unexpected(what)),
            Error::ExpectedVersion2 => f.write_str(messages::EXPECTED_VERSION_2),
            Error::Io(e) => fmt::Display::fmt(e, f),
        }
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> io::Error {
        match e {
            Error::Io(e) => e,
            other => io::Error::other(other),
        }
    }
}
