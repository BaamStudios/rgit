// SPDX-License-Identifier: MIT
//! Messages of the commands rgit only answers in part so far:
//! `checkout--worker` and `fsmonitor--daemon` (usage strings
//! and the messages of the paths that work, verbatim from the `git` binary).

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
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub const FSMONITOR_NOT_SUPPORTED: &str = "fsmonitor--daemon not supported on this platform";
pub const FSMONITOR_NOT_RUNNING: &str = "fsmonitor--daemon is not running";
pub fn fsmonitor_not_watching(work_tree: &str) -> String {
    format!("fsmonitor-daemon is not watching '{work_tree}'")
}
pub fn fsmonitor_watching(work_tree: &str) -> String {
    format!("fsmonitor-daemon is watching '{work_tree}'")
}
pub fn fsmonitor_already_running(work_tree: &str) -> String {
    format!("fsmonitor--daemon is already running '{work_tree}'")
}
/// rgit wording: the background daemon exited before it answered.
pub const FSMONITOR_FAILED_TO_START: &str = "fsmonitor--daemon failed to start";
/// rgit wording: the background daemon did not answer in time.
pub const FSMONITOR_NOT_ONLINE: &str = "fsmonitor--daemon not online yet";
/// rgit wording.
pub fn fsmonitor_could_not_spawn(err: &str) -> String {
    format!("could not spawn fsmonitor--daemon in the background: {err}")
}
/// rgit wording.
pub fn fsmonitor_could_not_watch(work_tree: &str, err: &str) -> String {
    format!("could not watch '{work_tree}': {err}")
}
/// rgit wording.
pub fn fsmonitor_could_not_listen(socket: &str, err: &str) -> String {
    format!("could not start IPC server on '{socket}': {err}")
}
pub fn fsmonitor_unhandled_subcommand(sub: &str) -> String {
    format!("Unhandled subcommand '{sub}'")
}
