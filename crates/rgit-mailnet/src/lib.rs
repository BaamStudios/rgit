// SPDX-License-Identifier: MIT
//! Mail transport for `send-email` and `imap-send`: a minimal SMTP client
//! (RFC 5321, STARTTLS per RFC 3207, AUTH per RFC 4954), a minimal IMAP
//! client (RFC 9051 / RFC 3501: LOGIN, AUTHENTICATE, LIST, APPEND), the
//! TLS setup they share, and the transfer encodings of RFC 2045
//! (base64, quoted-printable).
//!
//! Everything is synchronous; a connection is a plain TCP stream, a TLS
//! stream on rustls, or the pipes of a tunnel command.

pub mod encoding;
pub mod imap;
pub mod smtp;
pub mod stream;
pub mod tls;

pub use stream::{Log, Stream};
pub use tls::TlsOptions;
