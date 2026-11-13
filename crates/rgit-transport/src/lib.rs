// SPDX-License-Identifier: MIT
//! Transports connect rgit to a remote `git-upload-pack`,
//! `git-receive-pack` or `git-upload-archive` service.
//!
//! The protocol layer (protocol v0/v1/v2 clients) only sees a
//! [`Connection`]: a bidirectional byte stream carrying pkt-lines. How the
//! bytes travel is up to the [`Transport`]:
//!
//! - local paths and `file://` spawn the service program directly,
//! - `ssh://` and scp-like URLs run it through the system's `ssh`,
//! - smart HTTP (crate `rgit-http`) turns every request into a POST and is
//!   therefore [stateless](Connection::is_stateless).
//!
//! # Contract for a connection
//!
//! 1. After [`Transport::connect`] the first bytes readable from the
//!    connection are the server's initial response (ref advertisement or
//!    `version 2` capability advertisement) exactly as a stateful service
//!    sends it. A stateless transport removes its own framing (for HTTP: the
//!    `# service=<name>` packet and the following flush) before handing out
//!    the stream.
//! 2. The client writes one request and calls [`Connection::end_request`].
//!    A stateful transport merely flushes; a stateless one sends the
//!    buffered request and makes the response readable.
//! 3. The client reads the response, then continues with step 2 or calls
//!    [`Connection::close`].
//!
//! On a stateless connection the server keeps no state between requests,
//! so a v0/v1 fetch must resend its wants and common haves in every round
//! (`gitprotocol-http.adoc`), and protocol v2 commands must be complete
//! requests.
//!
//! # Protocol version negotiation
//!
//! The client asks for a protocol version through [`ConnectOptions`]; the
//! transport forwards [`ConnectOptions::git_protocol`] the way its medium
//! allows: the `GIT_PROTOCOL` environment variable for local and SSH (with
//! `-o SendEnv=GIT_PROTOCOL` for OpenSSH), the `Git-Protocol` header for
//! HTTP, the extra parameters of a `git://` request. Whether the server
//! honoured the request is only known from its response
//! ([`rgit_protocol::ProtocolVersion::read`]).

mod child;
pub mod command;
pub mod credential;
pub mod daemon;
mod error;
pub mod local;
pub mod messages;
mod os;
pub mod service;
pub mod sigpipe;
pub mod ssh;
pub mod url;

pub use child::ChildConnection;
pub use daemon::{DaemonTarget, DaemonTransport};
pub use error::{Error, Result};
pub use local::LocalTransport;
pub use service::{ConnectOptions, Service};
pub use ssh::{IpVersion, SshCommand, SshTransport, Variant};
pub use url::{Remote, SshTarget};

use std::ffi::OsString;
use std::io::{Read, Write};

use rgit_config::{ConfigSet, Environment};

/// A transport connection is the duplex stream of the protocol v2 client
/// (`rgit_protocol::v2::Client`).
impl rgit_protocol::v2::Duplex for Box<dyn Connection> {
    fn is_stateless(&self) -> bool {
        Connection::is_stateless(&**self)
    }

    fn end_request(&mut self) -> std::io::Result<()> {
        Connection::end_request(&mut **self).map_err(std::io::Error::other)
    }
}

/// Settings shared by the transports of this crate.
pub struct Settings<'a> {
    /// The environment (`GIT_SSH*`).
    pub env: &'a dyn Environment,
    /// The configuration (`core.sshCommand`, `ssh.variant`).
    pub config: &'a ConfigSet,
    /// `-4` / `-6`.
    pub ip: IpVersion,
    /// Extra environment for children (the global options' exports).
    pub base_env: Vec<(OsString, OsString)>,
}

/// Choose the transport of this crate for `url`: local paths and
/// `file://` get a [`LocalTransport`], SSH locations an [`SshTransport`],
/// `git://` URLs a [`DaemonTransport`]. Other schemes return `Ok(None)`
/// (smart HTTP lives in `rgit-http`, whose `transport_for` covers all).
pub fn transport_for(url: &str, settings: &Settings<'_>) -> Result<Option<Box<dyn Transport>>> {
    let exists = |p: &str| std::path::Path::new(p).exists();
    Ok(match url::parse_remote(url, &exists)? {
        Remote::Local { path } => Some(Box::new(LocalTransport {
            path,
            base_env: settings.base_env.clone(),
        })),
        Remote::Ssh(target) => Some(Box::new(SshTransport {
            target,
            ssh: SshCommand::from_config(settings.env, settings.config),
            ip: settings.ip,
            base_env: settings.base_env.clone(),
        })),
        Remote::Other { scheme } if scheme == "git" => {
            let rest = &url[url.find("://").map_or(0, |i| i + 3)..];
            Some(Box::new(DaemonTransport {
                target: DaemonTarget::parse(rest)?,
            }))
        }
        Remote::Other { .. } => None,
    })
}

/// A way to reach a remote repository.
///
/// A transport value is created for one remote location (URL plus the
/// configuration that applies to it) and can open connections to the
/// services of that repository.
pub trait Transport {
    /// Open a connection to `options.service` and return it once the
    /// server's initial response can be read.
    fn connect(&mut self, options: &ConnectOptions) -> Result<Box<dyn Connection>>;
}

/// A bidirectional stream to one remote service. See the
/// [crate documentation](crate) for the request/response contract.
pub trait Connection: Read + Write + Send {
    /// `true` if every request is answered independently (smart HTTP) and
    /// the server keeps no state between requests.
    fn is_stateless(&self) -> bool;

    /// Finish the current request. Stateful transports flush their output;
    /// stateless ones send the buffered request and make its response
    /// readable.
    fn end_request(&mut self) -> Result<()>;

    /// Separate read and write halves, for callers that read the response
    /// while still writing (e.g. `push` reading sideband progress while it
    /// sends the pack from another thread).
    fn split(&mut self) -> (&mut (dyn Read + Send), &mut (dyn Write + Send));

    /// Close the write side and wait for the remote service to finish
    /// (child process exit, end of the HTTP response). A service that
    /// failed is reported as [`Error::ServiceFailed`].
    fn close(self: Box<Self>) -> Result<()>;
}
