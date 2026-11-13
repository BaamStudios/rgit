// SPDX-License-Identifier: MIT
//! Creating commits: the pieces `write-tree`, `commit-tree` and `commit`
//! share.
//!
//! - [`write_tree`]: tree objects from the stage-0 entries of an index
//!   (Documentation/git-write-tree.adoc).
//! - [`create_commit`]: a commit object from a tree, parents, identities and
//!   a message (Documentation/git-commit-tree.adoc).
//! - [`message`]: commit message cleanup (`--cleanup`, `git-stripspace(1)`),
//!   the subject line and the empty-message rule of `git commit`.
//!
//! Errors are returned as data; the command prints git's messages.

pub mod message;
mod tree;

pub use tree::{WriteTreeError, WriteTreeOptions, write_tree};

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Signature};
use rgit_odb::ObjectDatabase;

/// What goes into a new commit object.
#[derive(Debug, Clone)]
pub struct NewCommit<'a> {
    pub tree: ObjectId,
    /// Parents in order; duplicates are the caller's business.
    pub parents: &'a [ObjectId],
    pub author: &'a Signature,
    pub committer: &'a Signature,
    /// The `encoding` header; `None` for UTF-8.
    pub encoding: Option<&'a [u8]>,
    /// The message exactly as stored.
    pub message: &'a [u8],
}

impl NewCommit<'_> {
    /// The commit object in git's canonical layout.
    pub fn to_commit(&self) -> Commit {
        Commit::new(
            self.tree,
            self.parents,
            self.author,
            self.committer,
            self.encoding,
            Vec::new(),
            self.message.to_vec(),
        )
    }
}

/// Write the commit object described by `c` and return its id.
pub fn create_commit(odb: &ObjectDatabase, c: &NewCommit<'_>) -> Result<ObjectId, rgit_odb::Error> {
    odb.write(ObjectKind::Commit, &c.to_commit().serialize())
}
