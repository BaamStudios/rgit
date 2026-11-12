// SPDX-License-Identifier: MIT
//! Revisions for rgit (ADR 0003: `rgit-revwalk`).
//!
//! * [`revision`]: the revision syntax of `gitrevisions(7)`: object names,
//!   ref names, suffixes such as `~<n>`, `^{<type>}`, `:<path>`, reflog and
//!   upstream forms, and ranges such as `A..B` and `A...B`.
//! * [`walk`]: walking the commit graph (`rev-list`, `log`).

pub mod revision;
pub mod walk;
