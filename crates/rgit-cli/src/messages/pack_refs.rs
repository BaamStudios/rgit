// SPDX-License-Identifier: MIT
//! Messages of `git pack-refs`, as printed by the reference git.

pub const USAGE: &[&str] =
    &["git pack-refs [--all] [--no-prune] [--auto] [--include <pattern>] [--exclude <pattern>]"];

pub const HELP_ALL: &str = "pack everything";
pub const HELP_PRUNE: &str = "prune loose refs (default)";
pub const HELP_AUTO: &str = "auto-pack refs as needed";
pub const HELP_INCLUDE: &str = "references to include";
pub const HELP_EXCLUDE: &str = "references to exclude";
