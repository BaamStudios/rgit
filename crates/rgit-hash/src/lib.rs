// SPDX-License-Identifier: MIT

//! Object ids and hashing for rgit.
//!
//! - [`HashKind`]: the hash algorithm (SHA-1 or SHA-256), its sizes, names
//!   and well-known ids (null, empty blob, empty tree).
//! - [`ObjectId`]: a 20 or 32 byte object name with hex parse/format,
//!   ordering and abbreviation ([`Prefix`] for short ids).
//! - [`Hasher`]: streaming hashing. SHA-1 always detects collision attacks
//!   and reports them as [`CollisionError`].
//! - [`hash_object`] / [`object_hasher`]: object ids over the
//!   `<type> <len>\0<content>` encoding.
//!
//! ```
//! use rgit_hash::{hash_object, HashKind, ObjectId};
//!
//! let id = hash_object(HashKind::Sha1, "blob", b"").unwrap();
//! assert_eq!(id, HashKind::Sha1.empty_blob());
//! assert_eq!(id.to_string(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
//! assert_eq!(format!("{id:.7}"), "e69de29");
//! assert_eq!("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".parse::<ObjectId>().unwrap(), id);
//! ```

mod error;
mod hasher;
mod kind;
mod oid;

pub use error::{CollisionError, Error, messages};
pub use hasher::{Hasher, hash, hash_object, object_hasher};
pub use kind::HashKind;
pub use oid::{ObjectId, Prefix};
