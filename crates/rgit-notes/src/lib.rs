// SPDX-License-Identifier: MIT
//! Notes: blobs attached to objects through a notes tree
//! (Documentation/git-notes.adoc, Documentation/config/notes.adoc).
//!
//! Details the documentation leaves open (the fan-out of written trees,
//! how notes found twice are combined, the exact text of the display) were
//! taken from the behaviour of the `git` binary (see CLEANROOM.md).
//!
//! - [`Notes`]: a notes tree in memory, read from and written to the object
//!   database with automatic fan-out, keeping non-note entries.
//! - [`Combine`]: what happens when a note meets an existing one
//!   (`notes.rewriteMode`, the merge strategies `union`/`cat_sort_uniq`).
//! - [`refname`]: notes ref names (`--ref`, `core.notesRef`,
//!   `GIT_NOTES_REF`, `notes.displayRef`).
//! - [`display`]: notes as `git log` shows them.

mod combine;
pub mod display;
mod error;
pub mod refname;
mod tree;

pub use combine::{Combine, cat_sort_uniq, concatenate};
pub use error::Error;
pub use tree::{NonNote, Notes};
