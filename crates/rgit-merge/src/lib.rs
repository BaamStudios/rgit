// SPDX-License-Identifier: MIT

//! Three-way merge of trees and commits with the results of git's default
//! merge strategy (`ort`), without rename detection.
//!
//! The merge runs in memory: it reads trees and blobs, writes the merged
//! blobs and trees, and returns the result tree, the conflicted paths with
//! their index stages and the messages git prints. Updating an index and a
//! work tree from that result is up to the caller (see
//! [`MergeResult::index_entries`]).
//!
//! # Behaviour
//!
//! Every path is merged on its own (a directory unchanged on one side is
//! taken from the other side as a whole):
//!
//! - unchanged on one side, or changed the same way on both: the other or
//!   the common version, silently;
//! - regular files changed on both sides: a content merge
//!   ([`rgit_diff::merge`], with histogram line diffs like `ort`) with the
//!   branch labels after the conflict
//!   markers ("Auto-merging", "CONFLICT (content)", "CONFLICT (add/add)"
//!   without a base); binary files keep our version; the executable bit
//!   is merged on its own;
//! - symbolic links and submodules changed on both sides: our version, in
//!   conflict;
//! - changed on one side and deleted on the other: the changed version
//!   ("CONFLICT (modify/delete)");
//! - different types (file, link, submodule) on the two sides: the regular
//!   file (or both, if neither is one) moves to `<path>~<branch>`
//!   ("CONFLICT (distinct types)");
//! - a file where the other side has a directory that is not empty after
//!   the merge: the file moves to `<path>~<branch>`
//!   ("CONFLICT (file/directory)").
//!
//! With several merge bases ([`merge_commits`]), the bases are merged
//! first, oldest first, into a virtual base ("Temporary merge branch 1/2",
//! conflict markers two characters longer per level). In those inner
//! merges a conflict that cannot be written with markers resolves to the
//! base version.
//!
//! The behaviour of `ort` is documented mostly through git's test suite;
//! the rules above were derived from `Documentation/git-merge.adoc`,
//! `Documentation/merge-strategies.adoc`, `Documentation/git-merge-tree.adoc`
//! and differential tests against the `git` binary (`merge-tree
//! --write-tree -X no-renames`).
//!
//! Not implemented yet: rename and directory rename detection (#151),
//! merge attributes and drivers, `-X` whitespace options (#153), merging
//! submodule histories (a submodule changed on both sides is always
//! reported as not checked out), `merge.renormalize`.

mod engine;
mod history;
pub mod messages;
#[cfg(test)]
mod tests;

pub use rgit_diff::Algorithm;
pub use rgit_diff::merge::{ConflictStyle, Favor};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{EntryMode, ObjectKind, RawObject};
use rgit_odb::ObjectDatabase;

pub use history::{CommitInfo, Commits, LabelFn, ObjectCommits, merge_bases, read_commit};

/// Where the merge reads and writes objects. Implemented for
/// [`ObjectDatabase`].
pub trait Objects {
    /// The hash algorithm of the repository.
    fn hash_kind(&self) -> HashKind;
    /// An object, `Ok(None)` when it does not exist. The error is a
    /// message.
    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, String>;
    /// Stores an object and returns its id.
    fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, String>;
}

impl Objects for ObjectDatabase {
    fn hash_kind(&self) -> HashKind {
        ObjectDatabase::hash_kind(self)
    }

    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
        ObjectDatabase::read(self, id).map_err(|e| e.to_string())
    }

    fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, String> {
        ObjectDatabase::write(self, kind, data).map_err(|e| e.to_string())
    }
}

/// Why a merge could not be carried out (conflicts are not errors).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{}", messages::missing_object(&.0.to_hex()))]
    MissingObject(ObjectId),
    #[error("{}", messages::not_a_tree(&.0.to_hex()))]
    NotATree(ObjectId),
    #[error("{}", messages::not_a_commit(&.0.to_hex()))]
    NotACommit(ObjectId),
    #[error("{}", messages::corrupt_object(&.0.to_hex(), .1))]
    Corrupt(ObjectId, String),
    #[error("{0}")]
    Read(String),
    #[error("{}", messages::write_failed(.0))]
    Write(String),
    /// [`merge_commits`] found no merge base and
    /// [`Options::allow_unrelated_histories`] is off.
    #[error("{}", messages::UNRELATED_HISTORIES)]
    UnrelatedHistories,
}

/// Options of a merge.
#[derive(Debug, Clone)]
pub struct Options {
    /// Label of our side ("HEAD", a branch name): after `<<<<<<<`, in
    /// messages and in `<path>~<label>`.
    pub branch1: Vec<u8>,
    /// Label of their side, after `>>>>>>>`.
    pub branch2: Vec<u8>,
    /// Label of the base after `|||||||` for [`merge_trees`];
    /// [`merge_commits`] sets it from the merge bases.
    pub ancestor: Vec<u8>,
    /// How content conflicts are written (`merge.conflictStyle`).
    pub style: ConflictStyle,
    /// Resolve content conflicts in favour of one side (`-X ours`,
    /// `-X theirs`).
    pub favor: Favor,
    /// Length of the conflict markers; 0 means the default (7).
    pub marker_size: usize,
    /// The line diff of content merges (`-X diff-algorithm=<name>`);
    /// `ort` uses the histogram algorithm.
    pub algorithm: Algorithm,
    /// With [`Algorithm::Myers`]: minimal diffs (`diff-algorithm=minimal`).
    pub minimal: bool,
    /// Let [`merge_commits`] merge histories without a common ancestor
    /// against the empty tree.
    pub allow_unrelated_histories: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            branch1: b"HEAD".to_vec(),
            branch2: Vec::new(),
            ancestor: Vec::new(),
            style: ConflictStyle::Merge,
            favor: Favor::None,
            marker_size: 0,
            algorithm: Algorithm::Histogram,
            minimal: false,
            allow_unrelated_histories: false,
        }
    }
}

/// A tree entry of one side: mode and object id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    /// The mode bits, canonical (`0o100644`, `0o100755`, `0o120000`,
    /// `0o160000`, or `0o040000` for a directory).
    pub mode: u32,
    pub oid: ObjectId,
}

impl Version {
    /// The mode as a tree entry mode.
    pub fn entry_mode(&self) -> EntryMode {
        EntryMode::from_bits(self.mode)
    }
}

/// A path left in conflict, with its versions in the base (index stage 1),
/// ours (2) and theirs (3); absent versions are `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictedPath {
    pub path: Vec<u8>,
    pub stages: [Option<Version>; 3],
}

/// The kind of a [`Message`]; [`MessageKind::as_str`] is the stable type
/// name `merge-tree -z` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    AutoMerging,
    Contents,
    Binary,
    FileDirectory,
    DistinctModes,
    ModifyDelete,
    SubmoduleNotInitialized,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageKind::AutoMerging => messages::TYPE_AUTO_MERGING,
            MessageKind::Contents => messages::TYPE_CONTENTS,
            MessageKind::Binary => messages::TYPE_BINARY,
            MessageKind::FileDirectory => messages::TYPE_FILE_DIRECTORY,
            MessageKind::DistinctModes => messages::TYPE_DISTINCT_MODES,
            MessageKind::ModifyDelete => messages::TYPE_MODIFY_DELETE,
            MessageKind::SubmoduleNotInitialized => messages::TYPE_SUBMODULE_NOT_INITIALIZED,
        }
    }
}

/// An informational or conflict message about one or more paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The paths the message is about; the first one orders the messages.
    pub paths: Vec<Vec<u8>>,
    pub kind: MessageKind,
    /// The text git prints (without a newline).
    pub text: Vec<u8>,
}

/// The result of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    /// The merged tree (written to the object database). Conflicted files
    /// hold conflict markers or our version.
    pub tree: ObjectId,
    /// Whether the merge had no conflicts.
    pub clean: bool,
    /// Conflicted paths, sorted by path.
    pub conflicts: Vec<ConflictedPath>,
    /// Messages in the order git prints them (by path).
    pub messages: Vec<Message>,
}

impl MergeResult {
    /// The index after the merge: the entries of [`MergeResult::tree`] at
    /// stage 0, with every conflicted path replaced by its stages 1 to 3.
    /// Entries carry no stat data; they are in index order.
    ///
    /// # Errors
    /// When the result tree cannot be read.
    pub fn index_entries(&self, objects: &dyn Objects) -> Result<Vec<rgit_index::Entry>, Error> {
        use rgit_index::{Entry, Mode, Stage};
        let mut out = Vec::new();
        engine::flatten_tree(objects, &self.tree, &mut Vec::new(), &mut |path, v| {
            out.push(Entry::new(
                path.to_vec(),
                Stage::MERGED,
                Mode(v.mode),
                v.oid,
            ));
        })?;
        let conflicted: std::collections::HashSet<&[u8]> =
            self.conflicts.iter().map(|c| c.path.as_slice()).collect();
        out.retain(|e| !conflicted.contains(e.path()));
        for c in &self.conflicts {
            for (i, v) in c.stages.iter().enumerate() {
                if let Some(v) = v {
                    let stage = Stage::new(i as u8 + 1).expect("stages 1 to 3");
                    out.push(Entry::new(c.path.clone(), stage, Mode(v.mode), v.oid));
                }
            }
        }
        out.sort_by(|a, b| a.path().cmp(b.path()).then(a.stage().cmp(&b.stage())));
        Ok(out)
    }
}

/// Merges the trees `ours` and `theirs` against the tree `base`.
///
/// The labels come from [`Options::branch1`], [`Options::branch2`] and
/// [`Options::ancestor`].
///
/// # Errors
/// When an object cannot be read or written.
pub fn merge_trees(
    objects: &dyn Objects,
    base: &ObjectId,
    ours: &ObjectId,
    theirs: &ObjectId,
    opts: &Options,
) -> Result<MergeResult, Error> {
    let labels = engine::Labels {
        ours: &opts.branch1,
        theirs: &opts.branch2,
        base: &opts.ancestor,
    };
    engine::merge(objects, opts, labels, 0, base, ours, theirs)
}

/// Merges the commits `ours` and `theirs` like `git merge`: against their
/// merge base, or against a virtual base merged from several bases.
///
/// # Errors
/// When an object cannot be read or written, and
/// [`Error::UnrelatedHistories`] when the commits have no common ancestor
/// and [`Options::allow_unrelated_histories`] is off.
pub fn merge_commits(
    objects: &dyn Objects,
    commits: &dyn Commits,
    ours: &ObjectId,
    theirs: &ObjectId,
    opts: &Options,
) -> Result<MergeResult, Error> {
    history::merge_recursive(objects, commits, ours, theirs, None, opts)
}

/// Like [`merge_commits`] with the merge bases given (in the order
/// [`merge_bases`] returns them), as `git merge` and the sequencer do.
///
/// # Errors
/// As [`merge_commits`].
pub fn merge_commits_with_bases(
    objects: &dyn Objects,
    commits: &dyn Commits,
    ours: &ObjectId,
    theirs: &ObjectId,
    bases: &[ObjectId],
    opts: &Options,
) -> Result<MergeResult, Error> {
    history::merge_recursive(objects, commits, ours, theirs, Some(bases), opts)
}
