// SPDX-License-Identifier: MIT

//! User-visible message texts, reproduced from the reference `test-tool` where
//! the test scripts depend on them (see `CLEANROOM.md`). Collected here so they
//! are reviewable in one place.

use std::fmt::Display;

/// First line of the subcommand list.
pub const USAGE: &str = "usage: test-tool <toolname> [args]";

/// Unknown subcommand.
pub fn no_such_tool(name: &str) -> String {
    format!("error: there is no tool named '{name}'")
}

/// `-C <dir>` failed.
pub fn cannot_chdir(dir: &str, _err: &dyn Display) -> String {
    format!("fatal: Could not cd to '{dir}'")
}

/// Prefix of fatal errors (exit code 128).
pub fn fatal(msg: impl Display) -> String {
    format!("fatal: {msg}")
}
