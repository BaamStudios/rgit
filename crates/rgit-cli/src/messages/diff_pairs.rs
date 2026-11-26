// SPDX-License-Identifier: MIT
//! Messages and usage of `git diff-pairs` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &str = "git diff-pairs -z [<diff-options>]";
pub const WITHOUT_Z: &str = "working without -z is not supported";
pub const PATHSPEC_NOT_SUPPORTED: &str = "pathspec arguments not supported";
pub const TREES_NOT_SUPPORTED: &str = "tree objects not supported";

pub const INVALID_RAW: &str = "invalid raw diff input";

pub fn bad_object_id(rest: &str) -> String {
    format!("unable to parse object id: {rest}")
}

pub fn unknown_status(status: &str) -> String {
    format!("unknown diff status: {status}")
}
