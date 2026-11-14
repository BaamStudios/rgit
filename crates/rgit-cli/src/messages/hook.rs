// SPDX-License-Identifier: MIT
//! Messages and usage of `git hook` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

pub const USAGE_RUN: &str = "git hook run [--allow-unknown-hook-name] [--ignore-missing] [--to-stdin=<path>] [(-j|--jobs) <n>]\n<hook-name> [-- <hook-args>]";
pub const USAGE_LIST: &str =
    "git hook list [--allow-unknown-hook-name] [-z] [--show-scope] <hook-name>";

pub const H_ALLOW_UNKNOWN: &str = "allow running a hook with a non-native hook name";
pub const H_IGNORE_MISSING: &str = "silently ignore missing requested <hook-name>";
pub const H_TO_STDIN: &str = "file to read into hooks' stdin";
pub const H_JOBS: &str = "run up to <n> hooks simultaneously (-1 for CPU count)";
pub const H_Z: &str = "use NUL as line terminator";
pub const H_SHOW_SCOPE: &str = "show the config scope that defined each hook";

pub const NO_EVENT_TO_LIST: &str = "you must specify a hook event name to list";

/// A `reference-transaction` hook refused the `prepared` phase.
pub fn transaction_aborted_by_hook(state: &str) -> String {
    format!("in '{state}' phase, update aborted by the reference-transaction hook")
}
