// SPDX-License-Identifier: MIT
//! Commit-graph files: reading, writing and verifying them, split chains
//! and changed-path Bloom filters.
//!
//! Written from `Documentation/gitformat-commit-graph.adoc`,
//! `Documentation/gitformat-chunk.adoc`,
//! `Documentation/technical/commit-graph.adoc`,
//! `Documentation/git-commit-graph.adoc`,
//! `Documentation/config/commitgraph.adoc` and the observable behaviour of
//! the `git` binary (files compared byte for byte, messages verbatim).
//!
//! - [`Graph`]: a loaded graph (one file or a chain of layers), commit
//!   lookup by id and by position, generation numbers, Bloom filters.
//! - [`write`]: writing a file or a new chain layer, merging layers,
//!   expiring unused layer files.
//! - [`verify`]: `git commit-graph verify`.
//! - [`bloom`]: murmur3, keys and filters.
//!
//! The crate does not read objects itself: writing and verifying take an
//! [`Objects`] implementation from the caller.

pub mod bloom;
mod file;
mod graph;
pub mod messages;
pub mod verify;
pub mod write;

pub use file::{FilterLookup, Layer, LayerError};
pub use graph::{
    ChainLines, CommitData, Graph, LoadOptions, ReadError, Source, chain_dir, chain_path,
    layer_path, read_chain, single_path,
};

use rgit_hash::ObjectId;

/// The file signature `CGPH`.
pub const SIGNATURE: u32 = 0x4347_5048;
/// Parent position meaning "no parent".
pub const NO_PARENT: u32 = 0x7000_0000;
/// Largest topological level that can be stored.
pub const GENERATION_V1_MAX: u32 = 0x3fff_ffff;

/// A message for the user, without the `error: `/`warning: ` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diag {
    /// Printed with `error: `.
    Error(String),
    /// Printed with `warning: `.
    Warning(String),
}

impl Diag {
    /// The line as git prints it (without the newline).
    pub fn line(&self) -> String {
        match self {
            Diag::Error(m) => format!("error: {m}"),
            Diag::Warning(m) => format!("warning: {m}"),
        }
    }
}

/// A commit read from the object database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommit {
    /// The root tree.
    pub tree: ObjectId,
    /// Parents in header order.
    pub parents: Vec<ObjectId>,
    /// Committer date; 0 when missing or unparsable.
    pub date: u64,
}

/// A commit object that exists but cannot be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unparsable;

/// Object access for writing and verifying.
pub trait Objects {
    /// Read and parse commit `id`. `Ok(None)`: missing or not a commit.
    /// `Err`: present but unparsable.
    fn commit(&self, id: &ObjectId) -> Result<Option<ParsedCommit>, Unparsable>;
    /// The entries of tree `id` (`None`: missing or not a tree).
    fn tree(&self, id: &ObjectId) -> Option<Vec<u8>>;
}
