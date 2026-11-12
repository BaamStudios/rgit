// SPDX-License-Identifier: MIT
//! Everything a command needs to know before it connects to another
//! repository: which URL, which refspecs, which branch tracks what.
//!
//! - [`url`]: URL classification ([`url::classify`]: remote helpers,
//!   `<scheme>://`, scp-like, local paths), the split of git's native
//!   transports ([`url::parse_connect_url`]: protocol, user and host, port,
//!   path, as `git fetch-pack --diag-url` shows them) and the clone
//!   directory name ([`url::humanish_dir`]).
//! - [`rewrite`]: `url.<base>.insteadOf` / `pushInsteadOf`.
//! - [`protocol`]: `protocol.allow`, `protocol.<name>.allow`,
//!   `GIT_ALLOW_PROTOCOL`, `GIT_PROTOCOL_FROM_USER`.
//! - [`refspec`]: parsing and validation for fetch and push, patterns,
//!   negative refspecs, forward and reverse mapping, the fetch ref map and
//!   the default refspecs of `git clone`.
//! - [`remote`]: `remote.<name>.*`, legacy `remotes/` and `branches/` files,
//!   default remote.
//! - [`branch`]: `branch.<name>.*`, `@{upstream}` and `@{push}`.
//!
//! Error values display git's messages verbatim (without the `fatal: `
//! prefix); see `messages.rs`.

pub mod branch;
mod error;
mod messages;
pub mod protocol;
pub mod refname;
pub mod refspec;
pub mod remote;
pub mod rewrite;
pub mod url;

pub use branch::{BranchConfig, PushDefault, TrackingError, push_ref, upstream_ref};
pub use error::ConfigError;
pub use protocol::{Policy, ProtocolError, check_allowed};
pub use refspec::{Direction, InvalidRefspec, Refspec, RefspecSet};
pub use remote::{Remote, RemoteError, RemoteSource, Remotes, TagOpt};
pub use rewrite::UrlRewrites;
pub use url::{ConnectUrl, ConnectUrlError, Protocol, Transport};
