// SPDX-License-Identifier: MIT

//! Writing packs (`git pack-objects`).
//!
//! Format: `Documentation/gitformat-pack.adoc`. Behaviour:
//! `Documentation/git-pack-objects.adoc`.
//!
//! - [`enumerate`]: the objects reachable from some tips and not from
//!   others (`rev-list --objects`), until the revision walker (#78) can do
//!   it.
//! - [`describe`]: type information for a plain list of object ids.
//! - [`pack_order`]: commits (with their tags), tags, trees, blobs.
//! - [`write_pack`]: a version 2 pack. Objects are copied from existing
//!   packs where possible (compressed whole objects, and deltas whose base
//!   is in the same output), the others go through a delta search over a
//!   sliding window (`search.rs`, deltas made by `encode.rs`) and are
//!   stored as `OFS_DELTA`/`REF_DELTA` or whole. [`write_thin_pack`] may
//!   also use objects the reader has as bases (`--thin`).
//! - [`PackSummary::indexed`]: the `.idx`/`.rev` content for a written
//!   pack.
//! - [`PackStream`]: the raw writer (header, entries, trailer) for callers
//!   that produce entries themselves.
//!
//! Objects are read through the [`ObjectSource`] trait, because the
//! object database is not a dependency of this crate; [`PackSetSource`]
//! covers packed objects.

mod encode;
mod entry;
mod error;
pub mod messages;
mod output;
mod pack;
mod search;
mod source;
mod walk;

pub use error::{SourceError, WriteError};
pub use output::PackStream;
pub use pack::{
    DEFAULT_BIG_FILE_THRESHOLD, DEFAULT_DEPTH, DEFAULT_WINDOW, MAX_DEPTH, PackSummary,
    WriteOptions, WrittenEntry, write_pack, write_pack_files, write_thin_pack,
};
pub use source::{ObjectSource, PackSetSource, ShallowSource};
pub use walk::{
    ListedObject, Tip, describe, enumerate, enumerate_with_edges, enumerate_with_edges_aggressive,
    name_hash, pack_order,
};
