// SPDX-License-Identifier: MIT
//! Error type of the transport layer.

use std::io;

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Transport errors. `Display` gives git's message without the `fatal: `
/// prefix where git has one.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// I/O on the connection failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// The service program or `ssh` could not be started.
    #[error("{message}")]
    Spawn {
        /// The complete message.
        message: String,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The remote service exited unsuccessfully (exit code, or `None` if
    /// it was killed by a signal).
    #[error("the remote end hung up unexpectedly")]
    ServiceFailed(Option<i32>),
    /// The URL or configuration cannot be used, e.g. a host name starting
    /// with `-`. The string is the complete message.
    #[error("{0}")]
    Refused(String),
    /// Protocol-level error from `rgit-protocol`.
    #[error(transparent)]
    Protocol(#[from] rgit_protocol::Error),
    /// Any other transport-specific failure (e.g. an HTTP status); the
    /// string is the complete message.
    #[error("{0}")]
    Other(String),
}
