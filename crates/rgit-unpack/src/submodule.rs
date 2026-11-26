// SPDX-License-Identifier: MIT

//! Recursing into submodules (`--recurse-submodules`, gitsubmodules(7)):
//! the hooks through which a merge checks and moves the work trees of the
//! active submodules along with their gitlinks.
//!
//! The merge itself only decides *which* submodule moves where; how a
//! submodule is checked and updated (running `read-tree` inside it,
//! connecting its git directory, absorbing an embedded `.git`) is up to the
//! caller's [`Submodules`] implementation.

use rgit_hash::ObjectId;

/// The caller's side of the submodule recursion.
///
/// The merge calls [`Submodules::active`] for every gitlink it touches.
/// For an active one it checks the move while planning
/// ([`Submodules::index_clean`], [`Submodules::check`]) and carries it out
/// in [`crate::Plan::apply`] ([`Submodules::update`]): removals before any
/// file is written, additions and changes after all files are written.
pub trait Submodules: std::fmt::Debug {
    /// Whether the submodule at `path` takes part in the recursion.
    fn active(&self, path: &[u8]) -> bool;

    /// Whether the submodule's index matches its `HEAD` (when it is
    /// populated). Prints the reason when it does not.
    fn index_clean(&self, path: &[u8]) -> bool;

    /// Dry run of moving the submodule at `path` from `old` to `new`
    /// (`None`: absent). `force` discards local changes. Prints the reason
    /// when it would fail.
    fn check(
        &self,
        path: &[u8],
        old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        force: bool,
    ) -> bool;

    /// Moves the submodule at `path` from `old` to `new`; on failure the
    /// message to report (without `error: `).
    ///
    /// # Errors
    /// The message when the submodule could not be moved.
    fn update(
        &self,
        path: &[u8],
        old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        force: bool,
    ) -> Result<(), String>;
}

/// One submodule the plan moves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Move {
    pub path: Vec<u8>,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}
