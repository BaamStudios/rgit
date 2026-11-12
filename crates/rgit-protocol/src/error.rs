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
    /// The server answered with protocol v0 (0) or v1 (1).
    NotV2(u8),
    /// The server advertised an unknown `object-format`.
    UnknownObjectFormat(String),
    /// The server does not advertise the command.
    CommandNotSupported(&'static str),
    /// v2 `fetch`: wrong first section (`expected`, `received`).
    ExpectedSection(&'static str, String),
    /// v2 `fetch`: bad line in the `acknowledgments` section.
    UnexpectedAckLine(String),
    /// v2 `fetch`: `ready` not followed by more sections.
    PackAfterReady,
    /// v2 `fetch`: more sections without `ready`.
    NoSectionsWithoutReady,
    /// v2 `fetch`: bad line in the `shallow-info` section.
    BadShallowLine(String),
    /// v2 `fetch`: bad line in the `wanted-refs` section.
    BadWantedRef(String),
    /// v2 `ls-refs`: bad line.
    InvalidLsRefs(String),
    /// The hash algorithm of the local repository differs from the
    /// server's.
    MismatchedAlgorithms {
        /// Local algorithm name.
        client: &'static str,
        /// Server algorithm name.
        server: &'static str,
    },
    /// Shallow/deepen arguments, but the server lacks `fetch=shallow`.
    ShallowNotSupported,
    /// A request line given by the caller contains LF or is too long.
    BadRequestLine(String),
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
            Error::NotV2(v) => f.write_str(&messages::not_v2(*v)),
            Error::UnknownObjectFormat(n) => f.write_str(&messages::unknown_object_format(n)),
            Error::CommandNotSupported(c) => f.write_str(&messages::command_not_supported(c)),
            Error::ExpectedSection(exp, got) => f.write_str(&messages::expected_section(exp, got)),
            Error::UnexpectedAckLine(l) => f.write_str(&messages::unexpected_ack_line(l)),
            Error::PackAfterReady => f.write_str(messages::PACK_AFTER_READY),
            Error::NoSectionsWithoutReady => f.write_str(messages::NO_SECTIONS_WITHOUT_READY),
            Error::BadShallowLine(l) => f.write_str(&messages::bad_shallow_line(l)),
            Error::BadWantedRef(l) => f.write_str(&messages::bad_wanted_ref(l)),
            Error::InvalidLsRefs(l) => f.write_str(&messages::invalid_ls_refs_response(l)),
            Error::MismatchedAlgorithms { client, server } => {
                f.write_str(&messages::mismatched_algorithms(client, server))
            }
            Error::ShallowNotSupported => f.write_str(messages::SHALLOW_NOT_SUPPORTED),
            Error::BadRequestLine(l) => f.write_str(&messages::bad_request_line(l)),
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
