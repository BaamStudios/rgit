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

/// The objects named by the `gc.recentObjectsHook` commands
/// (`config/gc.adoc`): each runs through the shell and prints one object
/// id per line; those that exist count as recent, whatever their age. A
/// failing hook or a line that is not an object id is fatal.
pub fn recent_objects_hook(
    config: &rgit_config::ConfigSet,
    odb: &rgit_odb::ObjectDatabase,
) -> Result<Vec<rgit_hash::ObjectId>, crate::Die> {
    let mut out = Vec::new();
    for entry in config.get_all("gc.recentobjectshook") {
        let Ok(cmd) = entry.string() else { continue };
        let cmd = cmd.to_string();
        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|e| crate::Die::Fatal(e.to_string()))?;
        if !result.status.success() {
            return Err(crate::Die::Fatal(
                crate::messages::housekeeping::hook_failed(&cmd),
            ));
        }
        for line in String::from_utf8_lossy(&result.stdout).lines() {
            let id = rgit_hash::ObjectId::from_hex(odb.hash_kind(), line.trim().as_bytes())
                .map_err(|_| {
                    crate::Die::Fatal(crate::messages::housekeeping::hook_bad_line(line))
                })?;
            if odb.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Seconds since the epoch of a file time (0 when unknown).
pub fn secs(t: std::io::Result<SystemTime>) -> u64 {
    t.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}
