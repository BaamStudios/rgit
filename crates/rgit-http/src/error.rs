// SPDX-License-Identifier: MIT
//! Errors of the smart HTTP client.
//!
//! [`Error`]'s `Display` is the text git prints after `fatal: ` (or
//! `error: ` for [`Error::Rpc`]). Plain-text error bodies from the server
//! are kept in [`Error::remote_lines`]; git prints them as `remote: <line>`
//! before the fatal message.

use crate::messages;

/// A transport-level failure, worded like libcurl, with libcurl's error
/// number (git includes it in `RPC failed; ... curl <n> ...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transport {
    /// libcurl-compatible error code (7 connect, 28 timeout, 60 certificate, ...).
    pub code: u32,
    /// Human-readable reason.
    pub reason: String,
}

impl Transport {
    pub(crate) fn new(code: u32, reason: impl Into<String>) -> Transport {
        Transport {
            code,
            reason: reason.into(),
        }
    }
}

/// Errors returned by [`crate::SmartHttp`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The URL could not be parsed.
    #[error("{}", messages::invalid_url(.0))]
    InvalidUrl(String),
    /// Not `http` or `https`.
    #[error("{}", messages::unsupported_scheme(.0))]
    UnsupportedScheme(String),
    /// 404 on discovery.
    #[error("{}", messages::repository_not_found(.url))]
    RepositoryNotFound {
        /// Repository URL as git prints it (with a trailing `/`).
        url: String,
    },
    /// 401 after credentials were tried (or none could be obtained).
    #[error("{}", messages::authentication_failed(.url))]
    AuthenticationFailed {
        /// Repository URL as git prints it.
        url: String,
    },
    /// Any other failure during discovery.
    #[error("{}", messages::unable_to_access(.url, .reason))]
    Access {
        /// Repository URL as git prints it.
        url: String,
        /// HTTP status, if the server answered.
        status: Option<u16>,
        /// libcurl-style reason.
        reason: String,
        /// Lines of a plain-text error body.
        remote: Vec<String>,
    },
    /// A non-smart reply that is not a dumb `info/refs` either.
    #[error("{}", messages::not_valid(.refs_url))]
    NotValid {
        /// `<repo>/info/refs` as git prints it.
        refs_url: String,
    },
    /// The server only speaks the dumb protocol.
    #[error("{}", messages::dumb_not_supported(.url))]
    DumbNotSupported {
        /// Repository URL.
        url: String,
    },
    /// A malformed smart advertisement (the text after `fatal: `).
    #[error("{0}")]
    InvalidServerResponse(String),
    /// A discovery redirect that does not end in the requested suffix.
    #[error("{}", messages::redirect_base(.asked, .redirect))]
    RedirectBase {
        /// The URL that was requested.
        asked: String,
        /// Where the server redirected to.
        redirect: String,
    },
    /// A failed POST (`error: RPC failed; ...`).
    #[error("{}", messages::rpc_failed(*.status, *.code, .reason))]
    Rpc {
        /// HTTP status, if the server answered.
        status: Option<u16>,
        /// libcurl-compatible code.
        code: u32,
        /// libcurl-style reason.
        reason: String,
        /// Lines of a plain-text error body.
        remote: Vec<String>,
    },
    /// The credential provider failed; its message is printed verbatim.
    #[error("{0}")]
    Credential(String),
    /// Invalid `http.*` configuration.
    #[error("{0}")]
    Config(String),
    /// Local I/O error (reading a request body).
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// Plain-text error body lines sent by the server, to be printed as
    /// `remote: <line>` before the error itself.
    pub fn remote_lines(&self) -> &[String] {
        match self {
            Error::Access { remote, .. } | Error::Rpc { remote, .. } => remote,
            _ => &[],
        }
    }

    /// The HTTP status, if the error carries one.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Access { status, .. } | Error::Rpc { status, .. } => *status,
            Error::RepositoryNotFound { .. } => Some(404),
            Error::AuthenticationFailed { .. } => Some(401),
            _ => None,
        }
    }
}

/// Result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;
