// SPDX-License-Identifier: MIT
//! The framing layer of git's wire protocol: pkt-lines, sideband
//! multiplexing, capability lists and the agent string.
//!
//! - [`pktline`]: [`Reader`] and [`Writer`] for pkt-lines, including flush
//!   (`0000`), delimiter (`0001`) and response-end (`0002`) packets and
//!   `ERR` packets.
//! - [`sideband`]: demultiplexing of `side-band`/`side-band-64k` streams,
//!   progress output on stderr as `remote: ...`, and the sending side.
//! - [`capabilities`]: capability lists of protocol v0/v1 (behind the NUL of
//!   the first ref line) and the v2 capability advertisement.
//! - [`agent`]: the `agent=` value rgit sends.
//! - [`trace`]: `GIT_TRACE_PACKET` output through `rgit-trace`.
//!
//! Behaviour follows `Documentation/gitprotocol-common.adoc`,
//! `gitprotocol-pack.adoc`, `gitprotocol-capabilities.adoc`,
//! `gitprotocol-v2.adoc` and observations of the git 2.56.0 binary.

pub mod agent;
pub mod capabilities;
mod error;
pub mod messages;
pub mod pktline;
pub mod sideband;
pub mod trace;

pub use capabilities::{Capabilities, ProtocolVersion};
pub use error::{Error, Result};
pub use pktline::{Packet, Reader, Writer};
pub use sideband::{Progress, ProgressStyle, SidebandReader};
