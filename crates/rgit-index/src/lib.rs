// SPDX-License-Identifier: MIT

//! The git index ("staging area", `$GIT_DIR/index`), as described in
//! `gitformat-index(5)`.
//!
//! - Reads versions 2, 3 and 4 (prefix-compressed paths) with SHA-1 or
//!   SHA-256 ids; verifies the trailing checksum (a null trailer from
//!   `index.skipHash` is accepted).
//! - Writes version 2, or 3 when an entry has extended flags
//!   (skip-worktree, intent-to-add), as git does; an index read as version
//!   4 is written as version 4.
//! - Extensions: `TREE` (cache tree) is parsed, invalidated on edits and
//!   written; `REUC` is kept as raw bytes. `UNTR`, `FSMN`, `EOIE`, `IEOT`
//!   and unknown optional (uppercase) extensions are dropped on write.
//!   `link` (split index), `sdir` (sparse index) and unknown required
//!   (lowercase) extensions are refused.
//! - In-memory editing: lookup, add/replace with file/directory conflict
//!   detection, removal, directory ranges, unmerged stages.
//! - On disk: `$GIT_INDEX_FILE`, writing through `index.lock`.
//!
//! ```
//! use rgit_hash::HashKind;
//! use rgit_index::{AddOptions, Index, Mode, ReadOptions, Stage, WriteOptions};
//!
//! let mut index = Index::new(HashKind::Sha1);
//! let blob = HashKind::Sha1.empty_blob();
//! index.add_path("src/main.rs", Mode::FILE, blob, AddOptions::default()).unwrap();
//! // "src" cannot become a file while "src/main.rs" exists.
//! assert!(index.add_path("src", Mode::FILE, blob, AddOptions::default()).is_err());
//!
//! let bytes = index.to_bytes(&WriteOptions::default());
//! let again = Index::from_bytes(&bytes, HashKind::Sha1, &ReadOptions::default()).unwrap();
//! assert_eq!(again.get(b"src/main.rs", Stage::MERGED).unwrap().id, blob);
//! ```

mod entry;
mod error;
mod file;
mod index;
pub mod messages;
mod read;
mod tree;
mod varint;
mod write;

pub use entry::{Entry, Flags, Mode, Stage, Stat, Time, verify_path};
pub use error::{AddError, Error, OrderError};
pub use file::{INDEX_FILE_ENV, WriteError, index_path};
pub use index::{AddOptions, Index};
pub use read::ReadOptions;
pub use tree::{CacheTree, TreeNode};
pub use write::WriteOptions;
