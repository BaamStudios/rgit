// SPDX-License-Identifier: MIT

//! Writing packs (`git pack-objects` without delta search).
//!
//! Format: `Documentation/gitformat-pack.adoc`. Behaviour:
//! `Documentation/git-pack-objects.adoc`.
//!
//! - [`enumerate`]: the objects reachable from some tips and not from
//!   others (`rev-list --objects`), until the revision walker (#78) can do
//!   it.
//! - [`describe`]: type information for a plain list of object ids.
//! - [`pack_order`]: commits (with their tags), tags, trees, blobs.
//! - [`write_pack`]: a version 2 pack. Objects are stored whole and
//!   zlib-compressed, or copied from existing packs: compressed whole
//!   objects, and deltas whose base is in the same output (written as
//!   `OFS_DELTA` or `REF_DELTA`). No new deltas are searched for (#19), so
//!   the output is valid and complete but usually larger than git's.
//!   Packs are never thin: every delta base is in the pack.
//! - [`PackStream`]: the raw writer (header, entries, trailer) for callers
//!   that produce entries themselves.
//!
//! Objects are read through the [`ObjectSource`] trait, because the
//! object database is not a dependency of this crate; [`PackSetSource`]
//! covers packed objects.

mod entry;
mod error;
pub mod messages;
mod output;
mod pack;
mod source;
mod walk;

pub use error::{SourceError, WriteError};
pub use output::PackStream;
pub use pack::{MAX_DEPTH, PackSummary, WriteOptions, WrittenEntry, write_pack};
pub use source::{ObjectSource, PackSetSource};
pub use walk::{ListedObject, Tip, describe, enumerate, pack_order};
