// SPDX-License-Identifier: MIT
//! Messages of the commands rgit only answers in part so far:
//! `backfill`, `checkout--worker` and `fsmonitor--daemon` (usage strings
//! and the messages of the paths that work, verbatim from the `git` binary).

// ------------------------------------------------------------- backfill

pub const BACKFILL_USAGE: &[&str] = &[
    "git backfill [--min-batch-size=<n>] [--[no-]sparse] [--[no-]include-edges] [<revision-range>]",
];
pub const BACKFILL_H_MIN_BATCH_SIZE: &str = "Minimum number of objects to request at a time";
pub const BACKFILL_H_SPARSE: &str = "Restrict the missing objects to the current sparse-checkout";
pub const BACKFILL_H_INCLUDE_EDGES: &str = "Include blobs from boundary commits in the backfill";
pub const BACKFILL_UNSUPPORTED: &str =
    "backfill from a promisor remote is not supported by rgit yet";

// ------------------------------------------------------ checkout--worker

pub const CHECKOUT_WORKER_USAGE: &[&str] = &["git checkout--worker [<options>]"];
pub const CHECKOUT_WORKER_H_PREFIX: &str = "when creating files, prepend <string>";
pub const HUNG_UP: &str = "the remote end hung up unexpectedly";
pub const CHECKOUT_WORKER_UNSUPPORTED: &str =
    "checkout--worker: parallel checkout is not supported by rgit";

// ----------------------------------------------------- fsmonitor--daemon

pub const FSMONITOR_USAGE: &[&str] = &[
    "git fsmonitor--daemon start [<options>]",
    "git fsmonitor--daemon run [<options>]",
    "git fsmonitor--daemon stop",
    "git fsmonitor--daemon status",
];
pub const FSMONITOR_H_DETACH: &str = "detach from console";
pub const FSMONITOR_H_IPC_THREADS: &str = "use <n> ipc worker threads";
pub const FSMONITOR_H_START_TIMEOUT: &str = "max seconds to wait for background daemon startup";
pub const FSMONITOR_NOT_SUPPORTED: &str = "fsmonitor--daemon not supported on this platform";
pub const FSMONITOR_NOT_RUNNING: &str = "fsmonitor--daemon is not running";
pub fn fsmonitor_not_watching(work_tree: &str) -> String {
    format!("fsmonitor-daemon is not watching '{work_tree}'")
}
pub fn fsmonitor_unhandled_subcommand(sub: &str) -> String {
    format!("Unhandled subcommand '{sub}'")
}
