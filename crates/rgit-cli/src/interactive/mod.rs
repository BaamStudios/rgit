// SPDX-License-Identifier: MIT
//! Interactive staging (Documentation/git-add.adoc, "INTERACTIVE MODE"
//! and "EDITING PATCHES"; Documentation/config/interactive.adoc).
//!
//! [`patch`] is the hunk selection shared by `add -p`, `reset -p`,
//! `checkout -p`, `restore -p`, `stash -p` and `commit -p`: the diff comes
//! from rgit's own `diff-files`/`diff-index`, the selected hunks go to
//! `apply`, as child processes like the steps the documentation
//! describes. `add -e` edits the whole diff.

pub(crate) mod edit;
pub(crate) mod hunks;
pub(crate) mod menu;
pub(crate) mod patch;

pub(crate) use patch::{Mode, Options};
