// SPDX-License-Identifier: MIT

//! The object database of rgit.
//!
//! - [`ObjectStore`]: the interface of an object source. A store hands out
//!   whole objects as [`RawObject`] (type + bytes), answers existence,
//!   header and prefix queries and lists its ids. Loose objects implement
//!   it here; the pack reader (#17) and alternates (#93) plug in the same
//!   way.
//! - [`LooseStore`]: loose objects under `objects/xx/…` (`gitformat-loose`).
//!   Reads inflate only as much as needed ([`LooseStore::header`]), stream
//!   large objects ([`LooseStore::open`]) and report corruption with git's
//!   messages ([`Error::git_lines`]). Writes compress with
//!   `core.looseCompression` / `core.compression`, go to a temp file that
//!   is renamed into place (mode 0444, adjusted for
//!   `core.sharedRepository`), optionally flushed per `core.fsync` /
//!   `core.fsyncMethod`, and are skipped when the object exists (its file
//!   time is refreshed instead). [`LooseStore::verify`] does fsck's checks.
//! - [`ObjectDatabase`]: an ordered list of stores plus the loose store new
//!   objects go to; lookups fall through to the next store when one copy is
//!   corrupt, and the skipped copy is reported on standard error like git.
//!   [`objects_dir`] honours `GIT_OBJECT_DIRECTORY`.
//!   **[`ObjectDatabase::open_for_repo`] is the one way for commands to
//!   read objects**: packs, loose objects and alternates.
//! - [`PackStore`]: the packs of a directory (`rgit-pack`) as a store.
//! - [`LooseOptions::from_config`]: the settings from the configuration.
//!
//! Compressed bytes written by rgit may differ from git's (a different
//! deflate implementation): the object path, the inflated bytes and
//! therefore the id are the same, and git and rgit read each other's
//! objects at every compression level.
//!
//! ```no_run
//! use rgit_hash::HashKind;
//! use rgit_object::ObjectKind;
//! use rgit_odb::{LooseOptions, ObjectDatabase};
//!
//! let odb = ObjectDatabase::open_with_packs(".git/objects", HashKind::Sha1, LooseOptions::default());
//! let id = odb.write(ObjectKind::Blob, b"hello\n").unwrap();
//! let obj = odb.read(&id).unwrap().unwrap();
//! assert_eq!(obj.data, b"hello\n");
//! ```

mod alternates;
mod db;
mod error;
mod loose;
pub mod messages;
mod options;
mod packed;
mod store;
mod zlib;

pub use alternates::{ALTERNATE_OBJECT_DIRECTORIES_ENV, resolve as resolve_alternates};
pub use db::{
    OBJECT_DIRECTORY_ENV, ObjectDatabase, PrefixMatch, ReadObjectError, objects_dir,
    print_error_to_stderr,
};
pub use error::Error;
pub use loose::{LooseReader, LooseStore};
pub use options::{DEFAULT_LOOSE_COMPRESSION, LooseOptions, ZLIB_DEFAULT_COMPRESSION, parse_fsync};
pub use packed::PackStore;
pub use store::{ObjectStore, RawObject};
