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
//! - [`write`]: writing packs (`pack-objects` without delta search).
//!
//! Objects come back as [`Object`] (= [`rgit_object::RawObject`]): the
//! type ([`rgit_object::ObjectKind`]) and the raw content bytes, without
//! parsing. SHA-1 and SHA-256 repositories are supported; the hash
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

/// The type of a stored object (the same type as everywhere in rgit).
pub use rgit_object::ObjectKind;

/// An object read from a pack: its type and content. This is
/// [`rgit_object::RawObject`], the form every object store hands out.
pub type Object = rgit_object::RawObject;

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
