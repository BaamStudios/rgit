// SPDX-License-Identifier: MIT
//! Messages and usage of `git backfill` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &[
    "git backfill [--min-batch-size=<n>] [--[no-]sparse] [--[no-]include-edges] [<revision-range>]",
];
pub const H_MIN_BATCH_SIZE: &str = "Minimum number of objects to request at a time";
pub const H_SPARSE: &str = "Restrict the missing objects to the current sparse-checkout";
pub const H_INCLUDE_EDGES: &str = "Include blobs from boundary commits in the backfill";

pub const PROBLEM_LOADING_SPARSE: &str = "problem loading sparse-checkout";
pub const BAD_FILTER: &str = "cannot backfill with these filter options";
pub const BLOB_LIMIT: &str = "cannot backfill with blob size limits";

pub fn unsupported_filter(spec: &str) -> String {
    format!("{spec} filter not supported by the path-walk API")
}

pub fn unrecognized_argument(arg: &str) -> String {
    format!("unrecognized argument: {arg}")
}

pub fn no_sparse_blob(spec: &str) -> String {
    format!("unable to access sparse blob in '{spec}'")
}
