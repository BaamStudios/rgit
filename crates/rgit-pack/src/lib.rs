// SPDX-License-Identifier: MIT

//! Reading git packs (`objects/pack/pack-*.{idx,pack}`).
//!
//! Format: `Documentation/gitformat-pack.adoc`.
//!
//! - [`PackIndex`]: `.idx` version 1 and 2 (including 64-bit offsets),
//!   lookup by full id and by abbreviated [`Prefix`](rgit_hash::Prefix).
//! - [`PackData`]: the `.pack` file, entry headers and zlib streams.
//! - [`Pack`]: index plus data; reads objects and resolves `OFS_DELTA` and
//!   `REF_DELTA` chains of any depth iteratively, using a bounded
//!   [`DeltaBaseCache`].
//! - [`PackSet`]: all packs of a directory, newest first, with rescan on a
//!   miss and fallback to another copy when one copy is corrupt.
//! - [`apply_delta`]: the delta format on its own.
//! - [`index_pack`]: index a pack stream (`git index-pack`): object ids,
//!   parallel delta resolution, thin pack completion, `.idx` and `.rev`
//!   output.
//! - [`write`]: writing packs (`pack-objects` without delta search).
//!
//! Objects come back as [`Object`]: the type and the raw content bytes,
//! without parsing. SHA-1 and SHA-256 repositories are supported; the hash
//! algorithm is passed in, because pack files do not record it.
//!
//! Files are memory mapped (the only `unsafe` code, in `mmap.rs`). Every
//! offset and size read from a file is bounds-checked: corrupt files give
//! [`Error`]s, never panics. `Error`'s `Display` is git's message text.

mod cache;
mod data;
mod delta;
mod error;
mod index;
pub mod index_pack;
pub mod messages;
mod mmap;
mod pack;
mod store;
pub mod write;

pub use cache::{DEFAULT_DELTA_BASE_CACHE_LIMIT, DeltaBaseCache};
pub use data::{EntryHeader, EntryKind, PACK_HEADER_LEN, PackData};
pub use delta::{DeltaError, apply_delta, delta_sizes};
pub use error::Error;
pub use index::PackIndex;
pub use pack::{ExternalBase, Pack};
pub use store::{PackSet, Report};

/// The type of a stored object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// A commit (pack type 1).
    Commit,
    /// A tree (pack type 2).
    Tree,
    /// A blob (pack type 3).
    Blob,
    /// An annotated tag (pack type 4).
    Tag,
}

impl ObjectKind {
    /// The type name used in object headers and by `git cat-file`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Commit => "commit",
            ObjectKind::Tree => "tree",
            ObjectKind::Blob => "blob",
            ObjectKind::Tag => "tag",
        }
    }

    /// The type number in pack entry headers.
    pub const fn pack_type(self) -> u8 {
        match self {
            ObjectKind::Commit => 1,
            ObjectKind::Tree => 2,
            ObjectKind::Blob => 3,
            ObjectKind::Tag => 4,
        }
    }
}

impl std::fmt::Display for ObjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An object read from a pack: its type and content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// The type.
    pub kind: ObjectKind,
    /// The content (without the `<type> <size>\0` header).
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn readers_are_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<crate::Pack>();
        check::<crate::PackSet>();
        check::<crate::PackIndex>();
        check::<crate::DeltaBaseCache>();
    }
}
