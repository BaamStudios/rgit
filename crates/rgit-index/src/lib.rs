// SPDX-License-Identifier: MIT

//! The git index ("staging area", `$GIT_DIR/index`), as described in
//! `gitformat-index(5)`.
//!
//! - Reads versions 2, 3 and 4 (prefix-compressed paths) with SHA-1 or
//!   SHA-256 ids. Like git, the trailing checksum is only verified on
//!   request ([`ReadOptions::verified`]); a null trailer from
//!   `index.skipHash` is always accepted. An index with an offset table
//!   (`IEOT`, found through `EOIE`) is parsed on several threads.
//! - Writes version 2, or 3 when an entry has extended flags
//!   (skip-worktree, intent-to-add), as git does; version 4 when the index
//!   is version 4 or, for a new index, when [`WriteOptions`] asks for it.
//!   Optionally with the End Of Index Entries and Index Entry Offset Table
//!   extensions laid out as git does.
//! - Extensions: `TREE` (cache tree) is parsed, invalidated on edits and
//!   written; `REUC` (resolve undo) is parsed, written, and filled whenever
//!   a conflict stage leaves the index; `UNTR` (untracked cache) is parsed,
//!   written and invalidated when paths appear in or vanish from the
//!   index. `FSMN` and unknown optional (uppercase) extensions are dropped
//!   on write. Unknown required (lowercase) extensions are refused.
//! - Sparse index (`sdir`): sparse directory entries are read and written;
//!   see [`Index::expand_sparse`] and [`Index::collapse_sparse`].
//! - Split index (`link`): read together with its `sharedindex.<hash>`;
//!   [`Index::write_file`] writes split as [`SplitOptions`] asks, with
//!   replaced, added and deleted entries against the shared index it was
//!   read with, or a new shared index.
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
mod ewah;
mod file;
mod index;
pub mod messages;
mod name_hash;
mod read;
mod sparse;
mod split;
mod tree;
mod untracked;
mod varint;
mod write;

pub use entry::{
    DEFAULT_MAX_PATH_LEN, Entry, Flags, Mode, Stage, Stat, Time, path_protection, set_max_path_len,
    set_path_protection, verify_path, verify_path_mode, verify_path_with,
};
pub use error::{AddError, Error, OrderError};
pub use file::{
    INDEX_FILE_ENV, WriteError, index_path, notify_written, set_write_observer, shared_index_name,
};
pub use index::{AddOptions, Index, ResolveUndo};
pub use name_hash::NameHash;
pub use read::ReadOptions;
pub use sparse::{
    CollapseRules, SparseOrigin, TreeReader, WriteTransform, has_tree_reader, set_tree_reader,
    set_write_transform,
};
pub use split::{SplitMode, SplitOptions};
pub use tree::{CacheTree, TreeNode};
pub use untracked::{CacheDir, CacheStat, ExcludeFile, UntrackedCache};
pub use write::{OffsetTable, WriteOptions};
