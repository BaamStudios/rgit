// SPDX-License-Identifier: MIT
//! Fetching from another repository (`git fetch`), and the building blocks
//! `clone` and `pull` share with it.
//!
//! Behaviour follows `Documentation/git-fetch.adoc`, `fetch-options.adoc`,
//! `pull-fetch-param.adoc`, `gitprotocol-v2.adoc` (`ls-refs`, `fetch`),
//! `gitprotocol-pack.adoc` (negotiation) and observations of git 2.56.
//! Only protocol v2 is spoken.
//!
//! # High level
//!
//! [`fetch`] runs the whole command for one remote: refspecs from the
//! command line or `remote.<name>.fetch`, `ls-refs` with `ref-prefix`
//! arguments derived from them, the ref map (opportunistic
//! remote-tracking updates, `branch.<name>.merge` candidates, tag
//! following), want/have negotiation with the `consecutive` algorithm,
//! the pack stored through index-pack (`--fix-thin`, kept until the refs
//! are written), the connectivity check, pruning, ref updates with the
//! fast-forward and tag rules and reflog entries, `refs/remotes/<name>/HEAD`
//! and `FETCH_HEAD`. The caller prints [`FetchResult::errors`] and
//! [`display::render`] of [`FetchResult::lines`].
//!
//! # Building blocks (for `clone`)
//!
//! - [`LocalRepo::open`]: objects, refs, reflog identity of a repository.
//! - [`Session::connect`]: transport for any supported URL (local path,
//!   `file://`, `ssh://`, `git://`, `http(s)://`), protocol v2 handshake;
//!   [`Session::ls_refs`]; [`Session::fetch_pack`] with `haves: None` for a
//!   clone (no negotiation); [`ReceivedPack::release`] after the refs are
//!   written.
//! - [`refmap`]: `ref-prefix` arguments and refspec mapping.
//! - [`update::decide`] / [`update::apply`]: one ref update with reflog
//!   message `<action>: <what>`.
//! - [`display::fetch_head`], [`display::display_url`].

pub mod display;
mod error;
mod fetch;
pub mod graph;
pub mod messages;
pub mod refmap;
mod repo;
mod session;
pub mod update;

pub use error::{Error, Result};
pub use fetch::{FetchOptions, FetchResult, fetch};
pub use repo::LocalRepo;
pub use session::{ConnectSettings, PackOptions, ReceivedPack, Session, protocol_name};
