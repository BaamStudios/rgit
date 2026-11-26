// SPDX-License-Identifier: MIT
//! Errors of `upload-pack`.

use std::io;

/// Why serving ended early.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A fatal condition. The text is git's message without `fatal: `;
    /// what the client is told about it (an `ERR` packet, a message on
    /// sideband 3) has been sent already.
    #[error("{0}")]
    Fatal(String),
    /// A request that is refused. The caller prints `error: <message>` and
    /// exits with 128; the client got an `ERR` packet.
    #[error("{0}")]
    Refused(String),
    /// Reading the request or writing the response failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl Error {
    pub(crate) fn fatal(msg: impl Into<String>) -> Error {
        Error::Fatal(msg.into())
    }
}

impl From<rgit_protocol::Error> for Error {
    fn from(e: rgit_protocol::Error) -> Error {
        match e {
            rgit_protocol::Error::Io(e) => Error::Io(e),
            other => Error::Fatal(other.to_string()),
        }
    }
}

/// Result alias for this module.
pub type Result<T, E = Error> = std::result::Result<T, E>;
