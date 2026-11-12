// SPDX-License-Identifier: MIT
//! Messages and the usage line of `git var` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &str = "usage: git var (-l | <variable>)";

pub fn invalid_branch_name(name: &str) -> String {
    format!("invalid branch name: init.defaultBranch = {name}")
}
