// SPDX-License-Identifier: MIT
//! Errors of a fetch. `Display` gives git's message without the
//! `fatal: ` prefix; every error is fatal (exit code 128) for the command.

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a fetch stopped.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A complete message (configuration, URL, ref name problems).
    #[error("{0}")]
    Fatal(String),
    /// The transport failed (connect, spawn, HTTP).
    #[error(transparent)]
    Transport(#[from] rgit_transport::Error),
    /// The protocol failed.
    #[error(transparent)]
    Protocol(#[from] rgit_protocol::Error),
    /// Receiving the pack failed.
    #[error(transparent)]
    IndexPack(#[from] rgit_pack::index_pack::Error),
    /// Reading the local repository failed.
    #[error(transparent)]
    Odb(#[from] rgit_odb::Error),
    /// Reading references failed.
    #[error(transparent)]
    Refs(#[from] rgit_refs::Error),
    /// Local I/O.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    /// A [`Error::Fatal`] with `msg`.
    pub fn fatal(msg: impl Into<String>) -> Error {
        Error::Fatal(msg.into())
    }
}
