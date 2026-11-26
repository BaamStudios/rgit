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

/// The git directory of a submodule lies within the git directory of
/// another one (`error: `).
pub fn git_dir_inside(sub: &str, outer: &str) -> String {
    format!("submodule git dir '{sub}' is inside git dir '{outer}'")
}

/// `hint: ` between that error and the fatal one.
pub const HINT_PATH_CONFIG: &str = "enabling extensions.submodulePathConfig might fix the following error, if it's not already enabled.";

/// `fatal: ` after [`git_dir_inside`] (two blanks as in git).
pub fn refusing_git_dir(sub: &str) -> String {
    format!("refusing to create/use '{sub}' in another submodule's  git dir.")
}
