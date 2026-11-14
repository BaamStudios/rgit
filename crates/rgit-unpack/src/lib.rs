// SPDX-License-Identifier: MIT

//! Tree unpacking: moving the index and the work tree from one tree to
//! another. This is the engine below `read-tree -m`, `checkout-index`,
//! and the porcelain built on them (`switch`/`checkout <branch>`,
//! `reset --hard`/`--merge`/`--keep`, the initial checkout of `clone`).
//!
//! Behaviour follows git-read-tree(1) ("Single Tree Merge", "Two Tree
//! Merge"), `Documentation/technical/trivial-merge.adoc` and
//! git-checkout-index(1), and was checked against the `git` binary with
//! differential tests.
//!
//! # Using it
//!
//! The work happens in two steps:
//!
//! 1. [`plan`] merges the trees into the index in memory and checks that
//!    nothing is lost. It returns a [`Plan`] (the new index plus the work
//!    tree changes), or [`Error::Rejected`] with the [`Problem`]s found.
//!    Nothing on disk is touched, so a dry run (`read-tree -n`) stops here.
//! 2. [`Plan::apply`] removes and writes the work tree files and fills in
//!    fresh stat data. The caller then writes the index ([`Applied::index`])
//!    through `index.lock` (smudging racily clean entries as usual).
//!
//! ```no_run
//! use rgit_unpack::{Identity, Merge, Options, Porcelain, Worktree, plan};
//! # fn demo(odb: &rgit_odb::ObjectDatabase, index: &rgit_index::Index,
//! #         root: &std::path::Path, checker: &rgit_worktree::stat::Checker,
//! #         excludes: &mut rgit_worktree::ignore::Excludes,
//! #         head: rgit_hash::ObjectId, target: rgit_hash::ObjectId) {
//! // `git switch <branch>`: two-way merge HEAD -> target, update the work
//! // tree, report all problems in porcelain wording.
//! let wt = Worktree { root, checker };
//! let mut opts = Options {
//!     update: true,
//!     show_all_errors: true,
//!     excludes: Some(excludes),
//!     ..Options::default()
//! };
//! match plan(odb, index, &Merge::TwoWay { old: head, new: target }, Some(&wt), &mut opts) {
//!     Ok(plan) => {
//!         let applied = plan.apply(odb, &wt, &Identity);
//!         // write applied.index; report applied.errors
//!     }
//!     Err(rgit_unpack::Error::Rejected(r)) => {
//!         for m in r.porcelain_messages(Porcelain::Checkout) {
//!             eprintln!("error: {m}");
//!         }
//!         eprintln!("{}", rgit_unpack::messages::ABORTING);
//!         // exit 1
//!     }
//!     Err(e) => eprintln!("fatal: {e}"),
//! }
//! # }
//! ```
//!
//! | Porcelain | [`Merge`] | [`Options`] |
//! |---|---|---|
//! | `switch`/`checkout <branch>` | `TwoWay { old: HEAD, new }` | `update`, `show_all_errors`, standard `excludes`; `initial_checkout` for an unborn index |
//! | `switch -f`, `checkout -f`, `reset --hard` | `OneWay(new)` | `reset`, `update` |
//! | `reset --mixed` (index only) | `OneWay(new)` | `reset` |
//! | `reset --merge` / `--keep` | `TwoWay` | `update`, `show_all_errors` |
//! | `clone` initial checkout | `OneWay(new)` on an empty index | `reset`, `update` |
//!
//! # The merge rules
//!
//! One-way (`read-tree -m <tree>`): the index becomes the tree; an entry
//! with the same mode and id keeps its stat data. Changed and removed
//! entries must be up to date in the work tree; new ones may not overwrite
//! untracked files (with `update`). With `reset` nothing is refused and,
//! with `update`, unchanged entries whose files differ are rewritten.
//!
//! Two-way (`read-tree -m <old> <new>`), per path, I = index, H = old,
//! M = new; "same" compares mode and id:
//!
//! | I | H, M | Result |
//! |---|---|---|
//! | none | M none | nothing |
//! | none | H = M | nothing (initial checkout: M) |
//! | none | H != M, both exist | refused ([`Problem::WouldOverwrite`]) unless initial checkout |
//! | none | only M | M (no untracked file in the way) |
//! | exists | H = M (or both missing), or I = M | keep I |
//! | I = H | M missing | remove (I must be up to date) |
//! | I = H | M differs | M (I must be up to date) |
//! | otherwise | | refused ([`Problem::WouldOverwrite`]) |
//!
//! With `reset` up-to-date and untracked checks are skipped; the
//! "otherwise" row is still refused.
//!
//! Replacing a directory by a file (with `update`) requires its tracked
//! files to be up to date; they are dropped from the index with it, and
//! untracked files in it that are not ignored refuse the merge
//! ([`Problem::NotUptodateDir`]). An untracked (or kept) file where a new
//! entry needs a directory is [`Problem::UntrackedOverwritten`]. Ignored
//! files (from [`Options::excludes`]) are overwritten.
//!
//! The new index keeps the old one's version and resolve-undo data; its
//! cache tree has a node for every directory, valid where the tree
//! object already exists ([`repair_cache_tree`]).
//!
//! # Work tree writes
//!
//! Removals come first (each followed by pruning directories that became
//! empty), then updates in index order. Leading files or symbolic links in
//! the way are removed, so nothing is written through a symbolic link.
//! Files are created with mode 0666 or 0777 (minus umask), symbolic links
//! as links or, with `core.symlinks=false`, as files holding the target;
//! gitlinks become empty directories. Regular file content goes through a
//! [`Filter`] (line endings, `ident`, `working-tree-encoding`).
//! [`write_entry`] and [`remove_entry`] are public for `checkout-index` and
//! path-wise checkouts.
//!
//! Tree paths are checked with [`verify_checkout_path`] (`.git` in any
//! case and its NTFS/HFS+ equivalents, `..`, symbolic `.gitmodules`);
//! a bad path ends the merge with [`Error::InvalidPath`]
//! (`error: invalid path '<path>'`).

mod cache_tree;
mod checkout;
pub mod messages;
mod objects;
mod path;
mod unpack;

pub use cache_tree::repair_cache_tree;
pub use checkout::{
    CheckoutError, Filter, Identity, create_leading_dirs, remove_empty_parents, remove_entry,
    write_entry,
};
pub use objects::{Objects, read_blob, tree_entries};
pub use path::{PathOptions, verify_checkout_path};
pub use unpack::{Applied, Merge, Options, Plan, Porcelain, Problem, Rejection, Worktree, plan};

use rgit_hash::ObjectId;

/// Why [`plan`] failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `-m` with conflicts in the index (`fatal: `).
    #[error("{}", messages::RESOLVE_INDEX_FIRST)]
    Unmerged,
    /// Local changes or untracked files would be lost.
    #[error("merge refused")]
    Rejected(Rejection),
    /// A tree path that may not be checked out (`error: `).
    #[error("{}", messages::invalid_path(&String::from_utf8_lossy(.0)))]
    InvalidPath(Vec<u8>),
    /// A tree could not be read.
    #[error("{}", messages::failed_to_unpack(&.0.to_hex()))]
    MissingTree(ObjectId),
    /// A tree could not be parsed (the parser's message).
    #[error("{0}")]
    CorruptTree(String),
    /// A blob could not be read.
    #[error("{}", messages::unable_to_read_sha1(&.0.to_hex()))]
    MissingBlob(ObjectId),
    /// Another I/O failure.
    #[error("{0}")]
    Io(String),
}
