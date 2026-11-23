// SPDX-License-Identifier: MIT

//! Three-way merge of trees and commits with the results of git's default
//! merge strategy (`ort`), with rename and directory rename detection.
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
//!   conflict (but see the submodule fast-forward below);
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
//! Renames ([`Options::renames`], on by default) are detected on each side
//! before the paths are merged; renamed files are merged at their new
//! path, with rename/delete, rename/rename, rename/add and directory
//! renames (`merge.directoryRenames`) reported like `ort` does. The
//! rules are described in the `renames` module.
//!
//! [`Options::whitespace`] makes content merges ignore whitespace changes
//! (`-X ignore-space-change` etc.), [`Options::subtree`] shifts their tree
//! and the base into our tree's shape first (`-s subtree`, `-X
//! subtree=<path>`).
//!
//! [`Options::path_merge`] supplies what depends on the repository: the
//! merge driver of a path (`text`, `binary`, `union` or a custom driver)
//! and its `conflict-marker-size`, renormalization ([`Options::renormalize`];
//! a file whose change was only the normalization counts as unchanged)
//! and the history of checked-out submodules (a submodule whose commit
//! on one side descends from the other's is fast-forwarded).
//! `-X ours`/`-X theirs` also pick a side of conflicting symbolic links.
//!
//! Not implemented yet: rename caching across a sequence of picks (a
//! performance feature), some reports of directory rename splits when
//! both sides renamed the same files.

mod engine;
mod history;
pub mod messages;
mod subtree;
#[cfg(test)]
mod tests;

pub use rgit_diff::merge::{ConflictStyle, Favor};
pub use rgit_diff::{Algorithm, Whitespace};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::{EntryMode, ObjectKind, RawObject};
use rgit_odb::ObjectDatabase;
pub use subtree::Subtree;

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
    /// A tree to merge is missing.
    #[error("{}", messages::could_not_read(&.0.to_hex()))]
    MissingTree(ObjectId),
    /// The trees to merge could not be walked: one is missing or a tree
    /// holds two entries of the same name. git reports
    /// [`Error::collect_errors`] and then this error's text.
    #[error("{}", messages::FAILURE_TO_MERGE)]
    Collect(Box<(CollectCause, [ObjectId; 3])>),
    /// A blob to merge is missing.
    #[error("{}", messages::missing_blob(&.0.to_hex()))]
    MissingBlob(ObjectId),
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
    /// A custom merge driver failed (it could not run, or it was killed).
    #[error("{0}")]
    Driver(String),
}

/// Why the trees to merge could not be walked ([`Error::Collect`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectCause {
    /// This tree is missing.
    Missing(ObjectId),
    /// A tree has two entries of this name (the path below the root).
    Duplicate(Vec<u8>),
}

impl Error {
    /// The `error:` lines git prints before the error's own text for an
    /// [`Error::Collect`] (without the `error: ` prefix).
    pub fn collect_errors(&self) -> Option<[String; 2]> {
        let Error::Collect(c) = self else {
            return None;
        };
        let (cause, trees) = &**c;
        let first = match cause {
            CollectCause::Missing(id) => messages::could_not_read(&id.to_hex()),
            CollectCause::Duplicate(path) => messages::duplicate_entries(path),
        };
        Some([
            first,
            messages::collecting_failed(&trees.map(|t| t.to_hex())),
        ])
    }
}

/// The merge driver of a path (the `merge` attribute, gitattributes(5)
/// "Performing a three-way merge").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Driver {
    /// The built-in three-way merge with conflict markers.
    Text,
    /// Our version, in conflict.
    Binary,
    /// The three-way merge taking the lines of both sides.
    Union,
    /// A configured custom driver, by name.
    Custom(Vec<u8>),
}

/// What a custom merge driver works on.
#[derive(Debug, Clone, Copy)]
pub struct DriverInput<'a> {
    pub path: &'a [u8],
    pub base: &'a [u8],
    pub ours: &'a [u8],
    pub theirs: &'a [u8],
    /// The conflict marker size.
    pub marker_size: usize,
    pub base_label: &'a [u8],
    pub ours_label: &'a [u8],
    pub theirs_label: &'a [u8],
}

/// Per-path merge settings that come from attributes and configuration,
/// provided by the caller.
pub trait PathMerge: std::fmt::Debug {
    /// The driver of `path` and its `conflict-marker-size` (`None`: the
    /// default). `inner` for the merges that build a virtual merge base
    /// (the driver's `recursive` setting). A custom driver is only
    /// returned when it is configured.
    fn driver(&self, path: &[u8], inner: bool) -> (Driver, Option<usize>);
    /// Runs the custom driver `name`: the merged content and whether it
    /// merged cleanly; an error when the driver could not be run or was
    /// killed.
    fn run(&self, name: &[u8], input: &DriverInput<'_>) -> Result<(Vec<u8>, bool), String>;
    /// The content `data` of `path` checked out and checked in again
    /// (`merge.renormalize`).
    fn renormalize(&self, _path: &[u8], data: Vec<u8>) -> Vec<u8> {
        data
    }
    /// Merge commits in the submodule at `path`, reachable from its
    /// references, that already contain both `ours` and `theirs` (the
    /// earliest such ones), as `"<abbrev> <subject>"` lines.
    fn submodule_merges(&self, _path: &[u8], _ours: &ObjectId, _theirs: &ObjectId) -> Vec<Vec<u8>> {
        Vec::new()
    }
    /// Whether commit `ancestor` is an ancestor of (or equal to)
    /// `descendant` in the submodule at `path`; `None` when the
    /// submodule is not checked out (or lacks one of the commits).
    fn submodule_is_ancestor(
        &self,
        _path: &[u8],
        _ancestor: &ObjectId,
        _descendant: &ObjectId,
    ) -> Option<bool> {
        None
    }
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
    /// Rename detection (`merge.renames`, `-X no-renames`); on by default.
    pub renames: bool,
    /// Minimum similarity of a rename (`-X find-renames=<n>`), in units
    /// of [`rgit_diff::diffcore::MAX_SCORE`].
    pub rename_score: u32,
    /// `merge.renameLimit`: sources times destinations above its square
    /// skip the exhaustive rename search; 0 is unlimited.
    pub rename_limit: usize,
    /// `merge.directoryRenames`.
    pub directory_renames: DirectoryRenames,
    /// Whitespace changes that do not count in content merges (`-X
    /// ignore-space-change`, `ignore-all-space`, `ignore-space-at-eol`,
    /// `ignore-cr-at-eol`).
    pub whitespace: Whitespace,
    /// Shift their tree and the base to our tree's shape first (`-s
    /// subtree`, `-X subtree=<path>`).
    pub subtree: Subtree,
    /// Merge attributes and drivers of paths; `None` merges every file
    /// with the built-in text driver.
    pub path_merge: Option<std::rc::Rc<dyn PathMerge>>,
    /// Normalize the three versions of a file with
    /// [`PathMerge::renormalize`] before merging them (`-X renormalize`,
    /// `merge.renormalize`).
    pub renormalize: bool,
}

/// What a merge does with a path the other side added to a directory
/// one side renamed (`merge.directoryRenames`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DirectoryRenames {
    /// Leave it in the old directory.
    False,
    /// Move it into the new directory.
    True,
    /// Move it, but report a conflict.
    #[default]
    Conflict,
}

/// `merge.renameLimit` when neither it nor `diff.renameLimit` is set.
pub const DEFAULT_RENAME_LIMIT: usize = 7000;

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
            renames: true,
            rename_score: rgit_diff::diffcore::DEFAULT_RENAME_SCORE,
            rename_limit: DEFAULT_RENAME_LIMIT,
            directory_renames: DirectoryRenames::Conflict,
            whitespace: Whitespace::default(),
            subtree: Subtree::None,
            path_merge: None,
            renormalize: false,
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
    RenameDelete,
    RenameRename,
    DirectoryRenameSuggested,
    PathUpdated,
    DirectoryRenameSplit,
    DirectoryRenameCollision,
    FileInTheWay,
    DirectoryRenameSkipped,
    RenameCollision,
    SubmoduleFastForward,
    SubmoduleConflict,
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
            MessageKind::RenameDelete => messages::TYPE_RENAME_DELETE,
            MessageKind::RenameRename => messages::TYPE_RENAME_RENAME,
            MessageKind::DirectoryRenameSuggested => messages::TYPE_DIRECTORY_RENAME_SUGGESTED,
            MessageKind::PathUpdated => messages::TYPE_PATH_UPDATED,
            MessageKind::DirectoryRenameSplit => messages::TYPE_DIRECTORY_RENAME_SPLIT,
            MessageKind::DirectoryRenameCollision => messages::TYPE_DIRECTORY_RENAME_COLLISION,
            MessageKind::FileInTheWay => messages::TYPE_FILE_IN_THE_WAY,
            MessageKind::DirectoryRenameSkipped => messages::TYPE_DIRECTORY_RENAME_SKIPPED,
            MessageKind::RenameCollision => messages::TYPE_RENAME_COLLISION,
            MessageKind::SubmoduleFastForward => messages::TYPE_SUBMODULE_FAST_FORWARD,
            MessageKind::SubmoduleConflict => messages::TYPE_SUBMODULE,
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
    /// The exhaustive rename search was skipped because of
    /// [`Options::rename_limit`]; the limit it would have needed (see
    /// [`messages::rename_limit_warnings`]).
    pub rename_limit_needed: Option<usize>,
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
    let (base, theirs) = subtree::shift_both(objects, &opts.subtree, ours, base, theirs)?;
    engine::merge(objects, opts, labels, 0, &base, ours, &theirs)
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
