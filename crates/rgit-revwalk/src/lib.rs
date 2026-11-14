// SPDX-License-Identifier: MIT
//! Revisions for rgit (ADR 0003: `rgit-revwalk`).
//!
//! * [`revision`]: the revision syntax of `gitrevisions(7)`: object names,
//!   ref names, suffixes such as `~<n>`, `^{<type>}`, `:<path>`, reflog and
//!   upstream forms, and ranges such as `A..B` and `A...B`.
//! * [`describe`]: the nearest named ancestor of a commit (`describe`).
//! * [`name_rev`]: names relative to references (`name-rev`).
//! * [`walk`]: walking the commit graph (`rev-list`, `log`).

pub mod describe;
pub mod name_rev;
pub mod revision;
pub mod walk;
