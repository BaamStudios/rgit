// SPDX-License-Identifier: MIT
//! Shared parts of the housekeeping commands `repack`, `prune`,
//! `prune-packed` and `gc`: which objects are reachable
//! ([`reach`]), the loose objects and packs of the repository
//! ([`store`]), writing a pack into `objects/pack` ([`store::write_pack`])
//! and the catalog files of `update-server-info` ([`server_info`]).

pub mod reach;
pub mod server_info;
pub mod store;

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the epoch of a file time (0 when unknown).
pub fn secs(t: std::io::Result<SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}
