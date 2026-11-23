// SPDX-License-Identifier: MIT

//! The in-memory index.

use std::ops::Range;

use rgit_hash::{HashKind, ObjectId};

use crate::entry::{Entry, Mode, Stage, verify_path};
use crate::error::{AddError, OrderError};
use crate::read::{self, ReadOptions};
use crate::write::{self, WriteOptions};
use crate::{CacheTree, Error};

/// Options for [`Index::add`].
#[derive(Clone, Copy, Debug, Default)]
pub struct AddOptions {
    /// Remove entries (of the same stage) that conflict with the new path
    /// as file and directory instead of failing
    /// (`git update-index --replace`).
    pub ok_to_replace: bool,
    /// Do not look for file/directory conflicts at all.
    pub skip_df_check: bool,
    /// Do not check the path with [`verify_path`].
    pub skip_path_check: bool,
}

/// The index: sorted entries plus extensions.
///
/// Entries are kept in index order (path bytes, then stage) by the editing
/// methods. An index read from disk is taken as it is; use
/// [`Index::verify_order`] to check it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub(crate) kind: HashKind,
    pub(crate) version: u32,
    pub(crate) entries: Vec<Entry>,
    pub(crate) tree: Option<CacheTree>,
    pub(crate) resolve_undo: Option<Vec<u8>>,
    pub(crate) ignored: Vec<[u8; 4]>,
    pub(crate) dropped: Vec<[u8; 4]>,
    pub(crate) checksum: Option<ObjectId>,
}

impl Index {
    /// An empty version 2 index.
    pub fn new(kind: HashKind) -> Index {
        Index {
            kind,
            version: 2,
            entries: Vec::new(),
            tree: None,
            resolve_undo: None,
            ignored: Vec::new(),
            dropped: Vec::new(),
            checksum: None,
        }
    }

    /// Parses an index file.
    pub fn from_bytes(data: &[u8], kind: HashKind, opts: &ReadOptions) -> Result<Index, Error> {
        read::parse(data, kind, opts)
    }

    /// Serializes the index (see [`Index::write_version`]).
    pub fn to_bytes(&self, opts: &WriteOptions) -> Vec<u8> {
        write::serialize(self, opts)
    }

    /// The hash algorithm of object ids and the checksum.
    pub fn hash_kind(&self) -> HashKind {
        self.kind
    }

    /// The format version as read (2, 3 or 4), or as set.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Sets the preferred format version; returns `false` (and changes
    /// nothing) unless it is 2, 3 or 4.
    pub fn set_version(&mut self, version: u32) -> bool {
        if (2..=4).contains(&version) {
            self.version = version;
            true
        } else {
            false
        }
    }

    /// The version [`Index::to_bytes`] writes: 4 if the index is version 4,
    /// otherwise 3 if any entry needs extended flags, else 2 (as git does).
    pub fn write_version(&self) -> u32 {
        if self.version == 4 {
            4
        } else if self.entries.iter().any(|e| e.flags.needs_extended()) {
            3
        } else {
            2
        }
    }

    /// The trailing checksum as read; `None` for a new index or a null
    /// trailer (`index.skipHash`).
    pub fn checksum(&self) -> Option<&ObjectId> {
        self.checksum.as_ref()
    }

    /// Unknown optional extensions skipped while reading, in file order.
    /// git prints [`crate::messages::ignoring_extension`] for each.
    pub fn ignored_extensions(&self) -> &[[u8; 4]] {
        &self.ignored
    }

    /// Known optional extensions that were read but are not kept and will
    /// not be written (`UNTR`, `FSMN`, `EOIE`, `IEOT`).
    pub fn dropped_extensions(&self) -> &[[u8; 4]] {
        &self.dropped
    }

    /// All entries in index order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True without entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Mutable access to an entry. Path and stage cannot change, so the
    /// order stays intact; call [`Index::invalidate_path`] when changing
    /// the id or mode (a pure stat refresh does not need it).
    pub fn entry_mut(&mut self, pos: usize) -> Option<&mut Entry> {
        self.entries.get_mut(pos)
    }

    /// Binary search for `path` at `stage`: `Ok(position)` if present,
    /// `Err(insert position)` otherwise.
    pub fn position(&self, path: &[u8], stage: Stage) -> Result<usize, usize> {
        self.entries.binary_search_by(|e| e.cmp_key(path, stage))
    }

    /// The entry for `path` at `stage`.
    pub fn get(&self, path: &[u8], stage: Stage) -> Option<&Entry> {
        self.position(path, stage).ok().map(|i| &self.entries[i])
    }

    /// The positions of all stages of `path`.
    pub fn path_range(&self, path: &[u8]) -> Range<usize> {
        let start = self.entries.partition_point(|e| e.path.as_slice() < path);
        let end = start + self.entries[start..].partition_point(|e| e.path == path);
        start..end
    }

    /// All entries (any stage) of `path`.
    pub fn entries_for_path(&self, path: &[u8]) -> &[Entry] {
        &self.entries[self.path_range(path)]
    }

    /// The positions of all entries below directory `dir` (`a/b` covers
    /// `a/b/...`, not `a/b` itself); the empty directory covers everything.
    pub fn dir_range(&self, dir: &[u8]) -> Range<usize> {
        if dir.is_empty() {
            return 0..self.entries.len();
        }
        let mut prefix = dir.to_vec();
        if prefix.last() != Some(&b'/') {
            prefix.push(b'/');
        }
        let start = self
            .entries
            .partition_point(|e| e.path.as_slice() < prefix.as_slice());
        let end = start + self.entries[start..].partition_point(|e| e.path.starts_with(&prefix));
        start..end
    }

    /// True if any entry has a stage other than 0.
    pub fn has_conflicts(&self) -> bool {
        self.entries.iter().any(|e| e.stage.is_conflict())
    }

    /// The entries of each conflicted path (all its stages, including a
    /// stage 0 entry if one exists), in index order.
    pub fn unmerged(&self) -> impl Iterator<Item = &[Entry]> {
        self.entries
            .chunk_by(|a, b| a.path == b.path)
            .filter(|group| group.iter().any(|e| e.stage.is_conflict()))
    }

    /// Adds `entry`, replacing an existing entry with the same path and
    /// stage. Other stages of the path are left alone; remove them with
    /// [`Index::remove`] when resolving a conflict.
    ///
    /// Fails if the path is invalid, or if the path is below an existing
    /// file or is an existing directory at the same stage (unless
    /// [`AddOptions::ok_to_replace`] removes those entries).
    pub fn add(&mut self, entry: Entry, opts: AddOptions) -> Result<(), AddError> {
        if !opts.skip_path_check && !verify_path(&entry.path) {
            return Err(AddError::InvalidPath(entry.path));
        }
        let stage = entry.stage;
        if !opts.skip_df_check {
            // Leading directories that exist as files.
            let mut files = Vec::new();
            for (i, _) in entry.path.iter().enumerate().filter(|(_, b)| **b == b'/') {
                if let Ok(pos) = self.position(&entry.path[..i], stage) {
                    files.push(pos);
                }
            }
            // Entries below the path.
            let below: Vec<usize> = self
                .dir_range(&entry.path)
                .filter(|&i| self.entries[i].stage == stage)
                .collect();
            if !files.is_empty() || !below.is_empty() {
                if !opts.ok_to_replace {
                    return Err(AddError::FileDirectoryConflict(entry.path));
                }
                let mut doomed: Vec<usize> = files.into_iter().chain(below).collect();
                doomed.sort_unstable();
                for pos in doomed.into_iter().rev() {
                    let old = self.entries.remove(pos);
                    self.invalidate_path(&old.path);
                }
            }
        }
        self.invalidate_path(&entry.path);
        match self.position(&entry.path, stage) {
            Ok(pos) => self.entries[pos] = entry,
            Err(pos) => self.entries.insert(pos, entry),
        }
        Ok(())
    }

    /// Convenience for [`Index::add`] with a new stage-0 entry.
    pub fn add_path(
        &mut self,
        path: impl Into<Vec<u8>>,
        mode: Mode,
        id: ObjectId,
        opts: AddOptions,
    ) -> Result<(), AddError> {
        self.add(Entry::new(path, Stage::MERGED, mode, id), opts)
    }

    /// Removes the entry at `pos`.
    pub fn remove_at(&mut self, pos: usize) -> Entry {
        let old = self.entries.remove(pos);
        self.invalidate_path(&old.path);
        old
    }

    /// Removes `path` at `stage`.
    pub fn remove(&mut self, path: &[u8], stage: Stage) -> Option<Entry> {
        let pos = self.position(path, stage).ok()?;
        Some(self.remove_at(pos))
    }

    /// Removes all stages of `path`.
    pub fn remove_path(&mut self, path: &[u8]) -> Vec<Entry> {
        let range = self.path_range(path);
        if !range.is_empty() {
            self.invalidate_path(path);
        }
        self.entries.drain(range).collect()
    }

    /// Removes every entry for which `keep` returns false.
    pub fn retain(&mut self, mut keep: impl FnMut(&Entry) -> bool) {
        let mut removed = Vec::new();
        self.entries.retain(|e| {
            let k = keep(e);
            if !k {
                removed.push(e.path.clone());
            }
            k
        });
        for path in removed {
            self.invalidate_path(&path);
        }
    }

    /// Checks that the entries are in index order, reporting the first
    /// problem as git does.
    pub fn verify_order(&self) -> Result<(), OrderError> {
        for pair in self.entries.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            match a.path.cmp(&b.path) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Greater => return Err(OrderError::Unordered),
                std::cmp::Ordering::Equal => {
                    if a.stage == Stage::MERGED {
                        return Err(OrderError::MultipleStages(a.path.clone()));
                    }
                    if a.stage > b.stage {
                        return Err(OrderError::UnorderedStages(a.path.clone()));
                    }
                }
            }
        }
        Ok(())
    }

    /// The cache tree, if the index has one.
    pub fn cache_tree(&self) -> Option<&CacheTree> {
        self.tree.as_ref()
    }

    /// The cache tree, mutable.
    pub fn cache_tree_mut(&mut self) -> Option<&mut CacheTree> {
        self.tree.as_mut()
    }

    /// Replaces (or with `None` removes) the cache tree.
    pub fn set_cache_tree(&mut self, tree: Option<CacheTree>) {
        self.tree = tree;
    }

    /// Invalidates the cache tree nodes of the directories containing
    /// `path`. Done automatically by the editing methods.
    pub fn invalidate_path(&mut self, path: &[u8]) {
        if let Some(tree) = &mut self.tree {
            tree.invalidate_path(path);
        }
    }

    /// The raw `REUC` (resolve undo) extension, kept unchanged from the
    /// file it was read from.
    pub fn resolve_undo_raw(&self) -> Option<&[u8]> {
        self.resolve_undo.as_deref()
    }

    /// The records of the resolve undo extension (gitformat-index(5),
    /// "Resolve undo"): per path the mode and id of stages 1-3 (`None` for
    /// a missing stage), sorted by path. A damaged extension ends the list.
    pub fn resolve_undo_records(&self) -> Vec<ResolveUndo> {
        let kind = self.hash_kind();
        let n = kind.byte_len();
        let mut records = Vec::new();
        let mut data = self.resolve_undo.as_deref().unwrap_or_default();
        let take = |data: &mut &[u8]| -> Option<Vec<u8>> {
            let nul = data.iter().position(|&b| b == 0)?;
            let s = data[..nul].to_vec();
            *data = &data[nul + 1..];
            Some(s)
        };
        'parse: while !data.is_empty() {
            let Some(path) = take(&mut data) else { break };
            let mut modes = [0u32; 3];
            for m in &mut modes {
                let Some(v) = take(&mut data)
                    .and_then(|s| String::from_utf8(s).ok())
                    .and_then(|s| u32::from_str_radix(&s, 8).ok())
                else {
                    break 'parse;
                };
                *m = v;
            }
            let mut stages = [None; 3];
            for (k, m) in modes.into_iter().enumerate() {
                if m != 0 {
                    let Some(id) = data
                        .get(..n)
                        .and_then(|b| ObjectId::from_bytes(kind, b).ok())
                    else {
                        break 'parse;
                    };
                    stages[k] = Some((m, id));
                    data = &data[n..];
                }
            }
            records.push(ResolveUndo { path, stages });
        }
        records
    }

    fn set_resolve_undo_records(&mut self, mut records: Vec<ResolveUndo>) {
        if records.is_empty() {
            self.resolve_undo = None;
            return;
        }
        records.sort_by(|a, b| a.path.cmp(&b.path));
        let mut out = Vec::new();
        for r in &records {
            out.extend_from_slice(&r.path);
            out.push(0);
            for s in &r.stages {
                out.extend_from_slice(format!("{:o}", s.map_or(0, |s| s.0)).as_bytes());
                out.push(0);
            }
            for (_, id) in r.stages.iter().flatten() {
                out.extend_from_slice(id.as_bytes());
            }
        }
        self.resolve_undo = Some(out);
    }

    /// Records the conflict stages 1-3 of `path` in the resolve undo
    /// extension, replacing an older record of the path.
    pub fn record_resolve_undo(&mut self, path: &[u8], stages: [Option<(u32, ObjectId)>; 3]) {
        let mut records = self.resolve_undo_records();
        records.retain(|r| r.path != path);
        records.push(ResolveUndo {
            path: path.to_vec(),
            stages,
        });
        self.set_resolve_undo_records(records);
    }

    /// Drops the resolve undo record of `path`.
    pub fn remove_resolve_undo(&mut self, path: &[u8]) {
        let mut records = self.resolve_undo_records();
        let before = records.len();
        records.retain(|r| r.path != path);
        if records.len() != before {
            self.set_resolve_undo_records(records);
        }
    }

    /// Drops the resolve undo extension.
    pub fn clear_resolve_undo(&mut self) {
        self.resolve_undo = None;
    }
}

/// One record of the resolve undo extension, see
/// [`Index::resolve_undo_records`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveUndo {
    pub path: Vec<u8>,
    /// Mode and id of stages 1, 2 and 3.
    pub stages: [Option<(u32, ObjectId)>; 3],
}
