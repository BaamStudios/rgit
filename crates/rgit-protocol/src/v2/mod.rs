// SPDX-License-Identifier: MIT
//! Protocol v2, client side (`Documentation/gitprotocol-v2.adoc`).
//!
//! - [`client`]: [`Client`] with `ls-refs` and `fetch` over a [`Duplex`]
//!   connection, stateful or stateless.
//! - [`negotiate`]: choosing `have` lines ([`ConsecutiveNegotiator`]).
//!
//! A transport asks for v2 by putting [`GIT_PROTOCOL_V2`] into the
//! `GIT_PROTOCOL` environment variable of the server process (`file://`,
//! `ssh://`), into the `Git-Protocol` header (HTTP), or as an extra
//! parameter of the `git://` request.

pub mod client;
pub mod negotiate;

#[cfg(test)]
mod tests;

pub use client::{
    Client, Duplex, FetchArgs, FetchOutcome, LsRefsArgs, Pipe, RemoteRef, ShallowInfo,
};
pub use negotiate::{CommitInfo, CommitSource, ConsecutiveNegotiator, Negotiator, NoopNegotiator};

/// The value of `GIT_PROTOCOL` / `Git-Protocol` that asks for protocol v2.
pub const GIT_PROTOCOL_V2: &str = "version=2";
