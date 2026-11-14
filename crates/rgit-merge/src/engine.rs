// SPDX-License-Identifier: MIT

//! The three-way merge of three trees against each other (one merge base).
//!
//! The trees are walked together directory by directory. A name whose
//! entry is unchanged on one side, or equal on both, is resolved without
//! looking inside (this also takes whole subdirectories). Otherwise the
//! directory part of the name is merged first, then the file part, so
//! that a file can move out of the way of a directory that is still there
//! after the merge.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rgit_diff::DiffOptions;
use rgit_diff::merge::{self as text, Favor, MergeOptions};
use rgit_hash::ObjectId;
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry, TreeIter};

use crate::{
    ConflictedPath, Error, MergeResult, Message, MessageKind, Objects, Options, Version, messages,
};

const MODE_TREE: u32 = 0o040000;
const TYPE_MASK: u32 = 0o170000;

/// The labels of a merge: after the conflict markers and in messages.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Labels<'a> {
    pub ours: &'a [u8],
    pub theirs: &'a [u8],
    pub base: &'a [u8],
}

/// The type of a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Regular,
    Link,
    Gitlink,
    Tree,
}

fn kind(mode: u32) -> Kind {
    match mode & TYPE_MASK {
        0o100000 => Kind::Regular,
        0o120000 => Kind::Link,
        0o040000 => Kind::Tree,
        _ => Kind::Gitlink,
    }
}

type Entry = Option<Version>;

/// How the file part of a path merges.
#[derive(Debug, Clone, Copy)]
enum Case {
    /// Resolved without conflict to this version (or deleted).
    Take(Entry),
    /// Changed on one side, deleted on the other.
    ModifyDelete,
    /// A file, a symbolic link or a submodule on the two sides.
    DistinctTypes,
    /// Changed on both sides, same type.
    Both(Kind),
}

/// Merges `ours` and `theirs` against `base` (all trees).
pub(crate) fn merge(
    objects: &dyn Objects,
    opts: &Options,
    labels: Labels<'_>,
    depth: usize,
    base: &ObjectId,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> Result<MergeResult, Error> {
    let mut e = Engine {
        objects,
        opts,
        labels,
        depth,
        conflicts: Vec::new(),
        conflicted: HashSet::new(),
        messages: Vec::new(),
        clean: true,
        deferred: [
            deletes_changed_file(objects, Some(*base), Some(*ours), Some(*theirs))?,
            deletes_changed_file(objects, Some(*base), Some(*theirs), Some(*ours))?,
        ],
    };
    let tree = if ours == theirs || base == theirs {
        *ours
    } else if base == ours {
        *theirs
    } else {
        match e.merge_dir(&mut Vec::new(), Some(*base), Some(*ours), Some(*theirs))? {
            Some(t) => t,
            None => e.write(ObjectKind::Tree, &[])?,
        }
    };
    let mut conflicts = e.conflicts;
    conflicts.sort_by(|a, b| a.path.cmp(&b.path));
    let mut messages = e.messages;
    messages.sort_by(|a, b| a.paths[0].cmp(&b.paths[0]));
    Ok(MergeResult {
        tree,
        clean: e.clean,
        conflicts,
        messages,
    })
}

struct Engine<'a> {
    objects: &'a dyn Objects,
    opts: &'a Options,
    labels: Labels<'a>,
    /// 0 for the merge itself, more for the merges of merge bases.
    depth: usize,
    conflicts: Vec<ConflictedPath>,
    /// The paths in `conflicts`.
    conflicted: HashSet<Vec<u8>>,
    messages: Vec<Message>,
    clean: bool,
    /// Per side (ours, theirs): whether the side deleted a file the other
    /// side changed. git then does not resolve the other paths that side
    /// deleted (and the directories it changed) up front, which shows in
    /// one place: a file deleted on that side to make room for a directory
    /// and unchanged on the other side is reported as moved out of the
    /// directory's way, although it stays deleted.
    deferred: [bool; 2],
}

/// One directory being merged: its path (with a trailing slash unless it
/// is the root), the names in use and the merged entries.
struct Dir {
    prefix: Vec<u8>,
    taken: BTreeSet<Vec<u8>>,
    out: Vec<(Vec<u8>, Version)>,
}

impl Dir {
    fn path(&self, name: &[u8]) -> Vec<u8> {
        [self.prefix.as_slice(), name].concat()
    }

    /// `<name>~<branch>`, with slashes in the branch replaced, made unique
    /// with `_0`, `_1`, ... among the names of the directory.
    fn unique_name(&mut self, name: &[u8], branch: &[u8]) -> Vec<u8> {
        let mut base = name.to_vec();
        base.push(b'~');
        base.extend(branch.iter().map(|&b| if b == b'/' { b'_' } else { b }));
        let mut candidate = base.clone();
        let mut n = 0usize;
        while self.taken.contains(&candidate) {
            candidate = base.clone();
            candidate.push(b'_');
            candidate.extend_from_slice(n.to_string().as_bytes());
            n += 1;
        }
        self.taken.insert(candidate.clone());
        candidate
    }
}

impl Engine<'_> {
    fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, Error> {
        self.objects.write(kind, data).map_err(Error::Write)
    }

    fn read_blob(&self, id: &ObjectId) -> Result<Vec<u8>, Error> {
        match self.objects.read(id).map_err(Error::Read)? {
            Some(raw) if raw.kind == ObjectKind::Blob => Ok(raw.data),
            Some(_) => Err(Error::Corrupt(*id, "not a blob".into())),
            None if *id == self.objects.hash_kind().empty_blob() => Ok(Vec::new()),
            None => Err(Error::MissingObject(*id)),
        }
    }

    fn message(&mut self, paths: Vec<Vec<u8>>, kind: MessageKind, text: Vec<u8>) {
        if self.depth == 0 {
            self.messages.push(Message { paths, kind, text });
        }
    }

    fn conflict(&mut self, path: Vec<u8>, stages: [Entry; 3]) {
        self.clean = false;
        if self.depth == 0 && self.conflicted.insert(path.clone()) {
            self.conflicts.push(ConflictedPath { path, stages });
        }
    }

    /// Merges one directory; `None` when the result is empty.
    fn merge_dir(
        &mut self,
        prefix: &mut Vec<u8>,
        base: Option<ObjectId>,
        ours: Option<ObjectId>,
        theirs: Option<ObjectId>,
    ) -> Result<Option<ObjectId>, Error> {
        let mut names: BTreeMap<Vec<u8>, [Entry; 3]> = BTreeMap::new();
        for (side, tree) in [base, ours, theirs].into_iter().enumerate() {
            if let Some(id) = tree {
                for (name, v) in read_tree(self.objects, &id)? {
                    names.entry(name).or_default()[side] = Some(v);
                }
            }
        }
        let mut dir = Dir {
            prefix: prefix.clone(),
            taken: names.keys().cloned().collect(),
            out: Vec::with_capacity(names.len()),
        };
        for (name, [b, o, t]) in names {
            if o == t {
                dir.out.extend(o.map(|v| (name, v)));
                continue;
            }
            let has_tree = |e: Entry| e.is_some_and(|v| kind(v.mode) == Kind::Tree);
            let either_tree = has_tree(b) || has_tree(o) || has_tree(t);
            if b == o && !(self.deferred[1] && either_tree) {
                dir.out.extend(t.map(|v| (name, v)));
                continue;
            }
            if b == t && !(self.deferred[0] && either_tree) {
                dir.out.extend(o.map(|v| (name, v)));
                continue;
            }
            let (bf, bd) = split(b);
            let (of, od) = split(o);
            let (tf, td) = split(t);
            let sub = if bd.is_some() || od.is_some() || td.is_some() {
                let len = prefix.len();
                prefix.extend_from_slice(&name);
                prefix.push(b'/');
                let r = self.merge_dir(prefix, bd, od, td);
                prefix.truncate(len);
                r?
            } else {
                None
            };
            if let Some(id) = sub {
                dir.out.push((
                    name.clone(),
                    Version {
                        mode: MODE_TREE,
                        oid: id,
                    },
                ));
            }
            if bf.is_none() && of.is_none() && tf.is_none() {
                continue;
            }
            let case = classify(bf, of, tf);
            if sub.is_some() && self.produces_entry(case, bf) {
                // The directory stays; the file moves out of its way.
                let branch = if of.is_some() {
                    self.labels.ours
                } else {
                    self.labels.theirs
                };
                let target = self.move_out_of_the_way(&mut dir, &name, branch);
                self.merge_file(&mut dir, &name, &target, case, [bf, of, tf])?;
                self.conflict(dir.path(&target), [bf, of, tf]);
                continue;
            }
            if sub.is_some() && bf.is_some() {
                // A deferred deletion (see `Engine::deferred`) is reported
                // as a move although the file stays deleted.
                if of.is_none() && tf == bf && self.deferred[0] {
                    self.move_out_of_the_way(&mut dir, &name, self.labels.theirs);
                } else if tf.is_none() && of == bf && self.deferred[1] {
                    self.move_out_of_the_way(&mut dir, &name, self.labels.ours);
                }
            }
            self.merge_file(&mut dir, &name, &name, case, [bf, of, tf])?;
        }
        if dir.out.is_empty() {
            return Ok(None);
        }
        let mut tree = Tree {
            entries: dir
                .out
                .into_iter()
                .map(|(name, v)| TreeEntry::new(EntryMode::from_bits(v.mode), name, v.oid))
                .collect(),
        };
        tree.sort();
        Ok(Some(self.write(ObjectKind::Tree, &tree.serialize())?))
    }

    /// Picks the name a file of `branch` moves to when a directory is in
    /// its way, and reports the move.
    fn move_out_of_the_way(&mut self, dir: &mut Dir, name: &[u8], branch: &[u8]) -> Vec<u8> {
        let target = dir.unique_name(name, branch);
        let (from, to) = (dir.path(name), dir.path(&target));
        let text = messages::file_directory(&from, branch, &to);
        self.message(vec![to, from], MessageKind::FileDirectory, text);
        target
    }

    /// Whether the file part of a path leaves a file behind.
    fn produces_entry(&self, case: Case, base: Entry) -> bool {
        match case {
            Case::Take(v) => v.is_some(),
            Case::ModifyDelete | Case::Both(Kind::Regular) => true,
            // In a virtual base these resolve to the base version.
            Case::DistinctTypes | Case::Both(_) => self.depth == 0 || base.is_some(),
        }
    }

    /// Merges the non-directory versions of `name`, writing the result
    /// to `target` (`name` unless it moved out of a directory's way).
    fn merge_file(
        &mut self,
        dir: &mut Dir,
        name: &[u8],
        target: &[u8],
        case: Case,
        stages: [Entry; 3],
    ) -> Result<(), Error> {
        let [b, o, t] = stages;
        let path = dir.path(target);
        match case {
            Case::Take(v) => {
                dir.out.extend(v.map(|v| (target.to_vec(), v)));
            }
            Case::ModifyDelete => {
                if self.depth > 0 {
                    dir.out.extend(b.map(|v| (target.to_vec(), v)));
                    return Ok(());
                }
                let (deleted_in, modified_in, kept) = match o {
                    None => (self.labels.ours, self.labels.theirs, t),
                    Some(_) => (self.labels.theirs, self.labels.ours, o),
                };
                dir.out.extend(kept.map(|v| (target.to_vec(), v)));
                let text = messages::modify_delete(&path, deleted_in, modified_in);
                self.message(vec![path.clone()], MessageKind::ModifyDelete, text);
                self.conflict(path, stages);
            }
            Case::DistinctTypes => {
                if self.depth > 0 {
                    dir.out.extend(b.map(|v| (target.to_vec(), v)));
                    return Ok(());
                }
                let (o, t) = (o.expect("both sides"), t.expect("both sides"));
                let (ko, kt) = (kind(o.mode), kind(t.mode));
                // The regular file moves; if neither is one, both move.
                let move_o = ko == Kind::Regular || kt != Kind::Regular;
                let move_t = kt == Kind::Regular || ko != Kind::Regular;
                let base_with = |k: Kind| b.filter(|b| kind(b.mode) == k);
                let o_name = if move_o {
                    dir.unique_name(name, self.labels.ours)
                } else {
                    target.to_vec()
                };
                let t_name = if move_t {
                    dir.unique_name(name, self.labels.theirs)
                } else {
                    target.to_vec()
                };
                let mut paths = vec![path.clone()];
                if move_o {
                    paths.push(dir.path(&o_name));
                }
                if move_t {
                    paths.push(dir.path(&t_name));
                }
                let text = messages::distinct_types(&path, move_o && move_t);
                self.message(paths, MessageKind::DistinctModes, text);
                self.conflict(dir.path(&o_name), [base_with(ko), Some(o), None]);
                self.conflict(dir.path(&t_name), [base_with(kt), None, Some(t)]);
                dir.out.push((o_name, o));
                dir.out.push((t_name, t));
            }
            Case::Both(Kind::Regular) => {
                let (o, t) = (o.expect("both sides"), t.expect("both sides"));
                let (merged, clean) = self.merge_regular(&path, b, o, t)?;
                dir.out.push((target.to_vec(), merged));
                if !clean {
                    self.conflict(path, stages);
                }
            }
            Case::Both(k) => {
                // Symbolic links and submodules cannot be merged.
                let o = o.expect("both sides");
                if self.depth > 0 {
                    dir.out.extend(b.map(|v| (target.to_vec(), v)));
                    return Ok(());
                }
                if k == Kind::Gitlink {
                    let text = messages::submodule_not_checked_out(&path);
                    self.message(
                        vec![path.clone()],
                        MessageKind::SubmoduleNotInitialized,
                        text,
                    );
                    let text = messages::submodule_conflict(&path);
                    self.message(vec![path.clone()], MessageKind::Contents, text);
                } else {
                    let text = messages::content_conflict(&path, b.is_none());
                    self.message(vec![path.clone()], MessageKind::Contents, text);
                }
                dir.out.push((target.to_vec(), o));
                self.conflict(path, stages);
            }
        }
        Ok(())
    }

    /// Merges two regular files: the executable bit and the content.
    /// Returns the result and whether it is clean.
    fn merge_regular(
        &mut self,
        path: &[u8],
        b: Entry,
        o: Version,
        t: Version,
    ) -> Result<(Version, bool), Error> {
        let base_mode = b.map(|b| b.mode);
        let (mode, mode_clean) = if o.mode == t.mode {
            (o.mode, true)
        } else if base_mode == Some(o.mode) {
            (t.mode, true)
        } else if base_mode == Some(t.mode) {
            (o.mode, true)
        } else {
            (o.mode, false)
        };
        let base_oid = b.map(|b| b.oid);
        let (oid, content_clean) = if o.oid == t.oid {
            (o.oid, true)
        } else if base_oid == Some(o.oid) {
            (t.oid, true)
        } else if base_oid == Some(t.oid) {
            (o.oid, true)
        } else {
            let r = self.merge_content(path, b, o, t)?;
            let text = messages::auto_merging(path);
            self.message(vec![path.to_vec()], MessageKind::AutoMerging, text);
            r
        };
        let clean = mode_clean && content_clean;
        if !clean {
            let text = messages::content_conflict(path, b.is_none());
            self.message(vec![path.to_vec()], MessageKind::Contents, text);
        }
        Ok((Version { mode, oid }, clean))
    }

    /// The three-way merge of the contents of two regular files.
    fn merge_content(
        &mut self,
        path: &[u8],
        b: Entry,
        o: Version,
        t: Version,
    ) -> Result<(ObjectId, bool), Error> {
        // A base of another type (a symbolic link) counts as empty.
        let base = match b {
            Some(b) if kind(b.mode) == Kind::Regular => self.read_blob(&b.oid)?,
            _ => Vec::new(),
        };
        let ours = self.read_blob(&o.oid)?;
        let theirs = self.read_blob(&t.oid)?;
        let marker_size = match self.opts.marker_size {
            0 => text::DEFAULT_MARKER_SIZE,
            n => n,
        } + 2 * self.depth;
        let mopts = MergeOptions {
            style: self.opts.style,
            favor: self.opts.favor,
            marker_size,
            ours_label: self.labels.ours,
            base_label: self.labels.base,
            theirs_label: self.labels.theirs,
            diff: DiffOptions {
                algorithm: self.opts.algorithm,
                minimal: self.opts.minimal,
                ..text::merge_diff_options()
            },
        };
        match text::merge(&base, &ours, &theirs, &mopts) {
            Ok(m) => Ok((self.write(ObjectKind::Blob, &m.content)?, m.is_clean())),
            Err(text::BinaryContent) => {
                if self.depth > 0 {
                    // A virtual base keeps the base version.
                    return Ok((self.write(ObjectKind::Blob, &base)?, false));
                }
                match self.opts.favor {
                    Favor::Ours => return Ok((o.oid, true)),
                    Favor::Theirs => return Ok((t.oid, true)),
                    _ => {}
                }
                let text = messages::binary(path, self.labels.ours, self.labels.theirs);
                self.message(vec![path.to_vec()], MessageKind::Binary, text);
                Ok((o.oid, false))
            }
        }
    }
}

/// Whether `side` deleted a file below the trees given that matters for
/// rename detection (git works this out even with renames off): one that
/// `other` did not keep unchanged, or one in a directory that `side`
/// removed and `other` added a file to.
fn deletes_changed_file(
    objects: &dyn Objects,
    base: Option<ObjectId>,
    side: Option<ObjectId>,
    other: Option<ObjectId>,
) -> Result<bool, Error> {
    let Some(base) = base else {
        return Ok(false);
    };
    if side == Some(base) || other == Some(base) {
        return Ok(false);
    }
    let entries = |id: Option<ObjectId>| -> Result<HashMap<Vec<u8>, Version>, Error> {
        match id {
            Some(id) => Ok(read_tree(objects, &id)?.into_iter().collect()),
            None => Ok(HashMap::new()),
        }
    };
    let side_entries = entries(side)?;
    let other_entries = entries(other)?;
    let base_entries = read_tree(objects, &base)?;
    if side.is_none() {
        // The directory is gone on `side`: a file `other` added to it
        // could follow a directory rename.
        let in_base: HashSet<&[u8]> = base_entries.iter().map(|(n, _)| n.as_slice()).collect();
        let added_file = other_entries
            .iter()
            .any(|(n, v)| kind(v.mode) != Kind::Tree && !in_base.contains(n.as_slice()));
        if added_file {
            return Ok(true);
        }
    }
    let tree_of = |e: Entry| e.filter(|v| kind(v.mode) == Kind::Tree).map(|v| v.oid);
    for (name, b) in &base_entries {
        let s = side_entries.get(name).copied();
        let o = other_entries.get(name).copied();
        if kind(b.mode) == Kind::Tree {
            if deletes_changed_file(objects, Some(b.oid), tree_of(s), tree_of(o))? {
                return Ok(true);
            }
        } else if s.is_none_or(|v| kind(v.mode) == Kind::Tree) && o != Some(*b) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Splits an entry into its file part and its directory part.
fn split(e: Entry) -> (Entry, Option<ObjectId>) {
    match e {
        Some(v) if kind(v.mode) == Kind::Tree => (None, Some(v.oid)),
        other => (other, None),
    }
}

fn classify(b: Entry, o: Entry, t: Entry) -> Case {
    if o == t {
        Case::Take(o)
    } else if b == o {
        Case::Take(t)
    } else if b == t {
        Case::Take(o)
    } else {
        match (o, t) {
            (Some(o), Some(t)) => {
                let (ko, kt) = (kind(o.mode), kind(t.mode));
                if ko == kt {
                    Case::Both(ko)
                } else {
                    Case::DistinctTypes
                }
            }
            _ => Case::ModifyDelete,
        }
    }
}

/// The entries of tree `id` with canonical modes.
pub(crate) fn read_tree(
    objects: &dyn Objects,
    id: &ObjectId,
) -> Result<Vec<(Vec<u8>, Version)>, Error> {
    let raw = match objects.read(id).map_err(Error::Read)? {
        Some(raw) if raw.kind == ObjectKind::Tree => raw,
        Some(_) => return Err(Error::NotATree(*id)),
        None if *id == objects.hash_kind().empty_tree() => return Ok(Vec::new()),
        None => return Err(Error::MissingObject(*id)),
    };
    let mut out = Vec::new();
    for item in TreeIter::new(objects.hash_kind(), &raw.data) {
        let e = item.map_err(|e| Error::Corrupt(*id, e.to_string()))?;
        let mode = EntryMode::from_bits(e.mode_bits).canonical().bits();
        out.push((e.name.to_vec(), Version { mode, oid: e.oid }));
    }
    Ok(out)
}

/// Calls `f` with every non-directory entry below tree `id`, in index
/// order of the paths.
pub(crate) fn flatten_tree(
    objects: &dyn Objects,
    id: &ObjectId,
    prefix: &mut Vec<u8>,
    f: &mut dyn FnMut(&[u8], Version),
) -> Result<(), Error> {
    let mut entries = read_tree(objects, id)?;
    // Index order compares whole paths: a directory sorts as if its name
    // ended in a slash, which is tree order.
    entries.sort_by(|a, b| {
        rgit_object::entry_cmp(
            &a.0,
            kind(a.1.mode) == Kind::Tree,
            &b.0,
            kind(b.1.mode) == Kind::Tree,
        )
    });
    for (name, v) in entries {
        let len = prefix.len();
        prefix.extend_from_slice(&name);
        if kind(v.mode) == Kind::Tree {
            prefix.push(b'/');
            flatten_tree(objects, &v.oid, prefix, f)?;
        } else {
            f(prefix, v);
        }
        prefix.truncate(len);
    }
    Ok(())
}
