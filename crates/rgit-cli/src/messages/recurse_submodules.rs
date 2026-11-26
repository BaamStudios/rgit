// SPDX-License-Identifier: MIT
//! Messages of `--recurse-submodules` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

/// The index of a submodule to be moved differs from its `HEAD` (`error: `).
pub fn dirty_index(path: &str) -> String {
    format!("submodule '{path}' has dirty index")
}

/// The dry run of a submodule move failed (`error: `).
pub fn could_not_update(path: &str) -> String {
    format!("Submodule '{path}' could not be updated.")
}

/// `--recurse-submodules=<value>` that is not a boolean (`fatal: `).
pub fn bad_recurse_value(value: &str) -> String {
    format!("bad recurse-submodules argument: {value}")
}
