// SPDX-License-Identifier: MIT
//! Messages of `git pack-refs`, as printed by the reference git.

pub const USAGE: &[&str] =
    &["git pack-refs [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]"];

pub const HELP_ALL: &str = "pack everything";
pub const HELP_PRUNE: &str = "prune loose refs (default)";
pub const HELP_AUTO: &str = "auto-pack refs as needed";
pub const HELP_INCLUDE: &str = "references to include";
pub const HELP_EXCLUDE: &str = "references to exclude";

/// `pack-refs` in a reftable repository could not merge the tables;
/// `reason` is e.g. `data is locked` or `entry too large`.
pub fn unable_to_compact(reason: &str) -> String {
    format!("unable to compact stack: {reason}")
}
