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

#[path = "renames.rs"]
mod renames;

use crate::{
    ConflictedPath, Driver, DriverInput, Error, MergeResult, Message, MessageKind, Objects,
    Options, Version, messages,
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
        deferred: [false, false],
        notes: renames::Notes::default(),
        extra_marker: 0,
        quiet_conflict: false,
        rename_limit_needed: None,
    };
    // git reads the three trees before anything else.
    for t in [base, ours, theirs] {
        if let Err(Error::MissingTree(missing)) = read_tree(objects, t) {
            return Err(Error::Collect(Box::new((missing, [*base, *ours, *theirs]))));
        }
    }
    let tree = if ours == theirs || base == theirs {
        *ours
    } else if base == ours {
        *theirs
    } else {
        let [base, ours, theirs] = if opts.renames {
            e.prepare_renames(base, ours, theirs)?
                .unwrap_or([*base, *ours, *theirs])
        } else {
            [*base, *ours, *theirs]
        };
        e.deferred = [
            deletes_changed_file(objects, Some(base), Some(ours), Some(theirs))?,
            deletes_changed_file(objects, Some(base), Some(theirs), Some(ours))?,
        ];
        match e.merge_dir(&mut Vec::new(), Some(base), Some(ours), Some(theirs))? {
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
        rename_limit_needed: e.rename_limit_needed,
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
    /// What the rename detection noted about paths.
    notes: renames::Notes,
    /// Added to the conflict marker length (rename/rename merges).
    extra_marker: usize,
    /// Do not report a content conflict (rename/rename merges).
    quiet_conflict: bool,
    rename_limit_needed: Option<usize>,
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
            None => Err(Error::MissingBlob(*id)),
        }
    }

    fn message(&mut self, paths: Vec<Vec<u8>>, kind: MessageKind, text: Vec<u8>) {
        if self.depth == 0 {
            self.messages.push(Message { paths, kind, text });
        }
    }

    fn conflict(&mut self, path: Vec<u8>, stages: [Entry; 3]) {
        self.clean = false;
        if self.depth > 0 {
            return;
        }
        if self.conflicted.insert(path.clone()) {
            self.conflicts.push(ConflictedPath { path, stages });
        } else if let Some(c) = self.conflicts.iter_mut().find(|c| c.path == path) {
            // Reported twice (a rename/rename target that the walk also
            // finds in conflict): the later stages fill in.
            for (old, new) in c.stages.iter_mut().zip(stages) {
                if new.is_some() {
                    *old = new;
                }
            }
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
        // The trees the rename detection writes can hold a file and a
        // directory of the same name; the directory part goes here then.
        let mut both: BTreeMap<Vec<u8>, [Option<ObjectId>; 3]> = BTreeMap::new();
        for (side, tree) in [base, ours, theirs].into_iter().enumerate() {
            if let Some(id) = tree {
                for (name, v) in read_tree(self.objects, &id)? {
                    let slot = &mut names.entry(name.clone()).or_default()[side];
                    match *slot {
                        Some(old) if kind(old.mode) == Kind::Tree => {
                            both.entry(name).or_default()[side] = Some(old.oid);
                            *slot = Some(v);
                        }
                        Some(_) => both.entry(name).or_default()[side] = Some(v.oid),
                        None => *slot = Some(v),
                    }
                }
            }
        }
        let mut dir = Dir {
            prefix: prefix.clone(),
            taken: names.keys().cloned().collect(),
            out: Vec::with_capacity(names.len()),
        };
        for (name, [b, o, t]) in names {
            let dual = both.get(&name).copied();
            let noted = dual.is_some() || self.notes.touches(&dir.path(&name));
            if o == t && !noted {
                dir.out.extend(o.map(|v| (name, v)));
                continue;
            }
            let has_tree = |e: Entry| e.is_some_and(|v| kind(v.mode) == Kind::Tree);
            let either_tree = has_tree(b) || has_tree(o) || has_tree(t);
            if b == o && !(self.deferred[1] && either_tree) && !noted {
                dir.out.extend(t.map(|v| (name, v)));
                continue;
            }
            if b == t && !(self.deferred[0] && either_tree) && !noted {
                dir.out.extend(o.map(|v| (name, v)));
                continue;
            }
            let (bf, mut bd) = split(b);
            let (of, mut od) = split(o);
            let (tf, mut td) = split(t);
            if let Some([db, d_o, dt]) = dual {
                bd = bd.or(db);
                od = od.or(d_o);
                td = td.or(dt);
            }
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
            let produces = self.produces_entry(case, bf)
                || self.notes.rename_delete.contains(&dir.path(&name));
            if sub.is_some() && produces {
                // The directory stays; the file moves out of its way. A
                // file renamed here belongs to the side that renamed it.
                let path = dir.path(&name);
                let renamed_by = self
                    .notes
                    .labels
                    .get(&path)
                    .and_then(|p| (1..3).find(|&k| p[k] == path && p[3 - k] != path));
                let branch = match renamed_by {
                    Some(1) => self.labels.ours,
                    Some(_) => self.labels.theirs,
                    None if of.is_some() => self.labels.ours,
                    None => self.labels.theirs,
                };
                let target = self.move_out_of_the_way(&mut dir, &name, branch);
                let moved = dir.path(&target);
                if let Some(l) = self.notes.labels.get(&path).cloned() {
                    self.notes.labels.insert(moved.clone(), l);
                }
                // A conflict reported before the walk (rename/rename)
                // moves along.
                let earlier = self.conflicts.iter().position(|c| c.path == path).map(|i| {
                    self.conflicted.remove(&path);
                    self.conflicts.remove(i).stages
                });
                self.merge_file(&mut dir, &name, &target, case, [bf, of, tf])?;
                if let Some(stages) = earlier {
                    self.conflict(moved, stages);
                    continue;
                }
                match renamed_by {
                    Some(k) if !self.conflicted.contains(&moved) => {
                        // Only the merged version, at the renaming side.
                        let merged = dir
                            .out
                            .iter()
                            .rev()
                            .find(|(n, _)| *n == target)
                            .map(|e| e.1);
                        let mut stages = [None; 3];
                        stages[k] = merged;
                        self.conflict(moved, stages);
                    }
                    _ => self.conflict(moved, [bf, of, tf]),
                }
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
        let noted_path = dir.path(name);
        if let Case::Take(_) = case
            && self.notes.rename_delete.contains(&noted_path)
        {
            // Renamed unchanged on one side, deleted on the other.
            let kept = if self.depth > 0 { b } else { o.or(t) };
            dir.out.extend(kept.map(|v| (target.to_vec(), v)));
            self.conflict(path, stages);
            return Ok(());
        }
        self.merge_file_inner(dir, name, target, case, stages)?;
        if self.notes.location.contains(&noted_path) && !self.conflicted.contains(&path) {
            // Moved by a directory rename: a conflict even when clean.
            self.conflict(path, stages);
        }
        Ok(())
    }

    fn merge_file_inner(
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
                if self.unchanged_after_renormalizing(&path, b, kept)? {
                    // Only the normalization changed: the deletion wins.
                    return Ok(());
                }
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
                if k == Kind::Link {
                    // -Xours / -Xtheirs pick a side.
                    let pick = match self.opts.favor {
                        Favor::Ours => Some(o),
                        Favor::Theirs => t,
                        _ => None,
                    };
                    if let Some(v) = pick {
                        dir.out.push((target.to_vec(), v));
                        return Ok(());
                    }
                }
                if k == Kind::Gitlink {
                    if let Some(v) = self.merge_submodule(&path, b, o, t)? {
                        dir.out.push((target.to_vec(), v));
                        return Ok(());
                    }
                }
                if k == Kind::Gitlink && self.submodule_unavailable(&path, o) {
                    let text = messages::submodule_not_checked_out(&path);
                    self.message(
                        vec![path.clone()],
                        MessageKind::SubmoduleNotInitialized,
                        text,
                    );
                    let text = messages::submodule_conflict(&path);
                    self.message(vec![path.clone()], MessageKind::Contents, text);
                } else if k == Kind::Gitlink {
                    let text = self.submodule_failure(&path, b, o, t);
                    self.message(vec![path.clone()], MessageKind::SubmoduleConflict, text);
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

    /// Whether the submodule at `path` cannot be looked into (it is not
    /// checked out, or there are no merge attributes to ask).
    fn submodule_unavailable(&self, path: &[u8], o: Version) -> bool {
        match &self.opts.path_merge {
            Some(pm) => pm.submodule_is_ancestor(path, &o.oid, &o.oid).is_none(),
            None => true,
        }
    }

    /// Submodule commits changed on both sides: when one descends from
    /// the other (and both from the base), the descendant
    /// ("Fast-forwarding submodule"); `None` leaves the conflict.
    fn merge_submodule(
        &mut self,
        path: &[u8],
        b: Entry,
        o: Version,
        t: Entry,
    ) -> Result<Option<Version>, Error> {
        let (Some(pm), Some(b), Some(t)) = (&self.opts.path_merge, b, t) else {
            return Ok(None);
        };
        if kind(b.mode) != Kind::Gitlink || kind(t.mode) != Kind::Gitlink {
            return Ok(None);
        }
        let anc = |a: &ObjectId, d: &ObjectId| pm.submodule_is_ancestor(path, a, d) == Some(true);
        if !anc(&b.oid, &o.oid) || !anc(&b.oid, &t.oid) {
            return Ok(None);
        }
        let pick = if anc(&o.oid, &t.oid) {
            t
        } else if anc(&t.oid, &o.oid) {
            o
        } else {
            return Ok(None);
        };
        let text = messages::submodule_fast_forward(path, &pick.oid.to_hex());
        self.message(vec![path.to_vec()], MessageKind::SubmoduleFastForward, text);
        Ok(Some(pick))
    }

    /// Why the submodule commits at `path` could not be merged: they do
    /// not both descend from the base's, or merge commits in the
    /// submodule that combine them are suggested.
    fn submodule_failure(&self, path: &[u8], b: Entry, o: Version, t: Entry) -> Vec<u8> {
        let (Some(pm), Some(b), Some(t)) = (&self.opts.path_merge, b, t) else {
            return messages::submodule_failed(path);
        };
        if kind(b.mode) == Kind::Gitlink && kind(t.mode) == Kind::Gitlink {
            let behind =
                |a: &ObjectId, d: &ObjectId| pm.submodule_is_ancestor(path, a, d) == Some(false);
            if behind(&b.oid, &o.oid) || behind(&b.oid, &t.oid) {
                return messages::submodule_not_following(path);
            }
        }
        let lines = pm.submodule_merges(path, &o.oid, &t.oid);
        if lines.is_empty() {
            messages::submodule_failed(path)
        } else {
            messages::submodule_resolutions(path, &lines)
        }
    }

    /// With renormalization: whether the regular files `b` and `v` of
    /// `path` are equal once both are normalized.
    fn unchanged_after_renormalizing(
        &self,
        path: &[u8],
        b: Entry,
        v: Entry,
    ) -> Result<bool, Error> {
        let (Some(pm), Some(b), Some(v)) = (&self.opts.path_merge, b, v) else {
            return Ok(false);
        };
        if !self.opts.renormalize || kind(b.mode) != Kind::Regular || kind(v.mode) != Kind::Regular
        {
            return Ok(false);
        }
        let base = pm.renormalize(path, self.read_blob(&b.oid)?);
        let data = pm.renormalize(path, self.read_blob(&v.oid)?);
        Ok(base == data)
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
        if !clean && !self.quiet_conflict {
            let text = messages::content_conflict(path, b.is_none());
            self.message(vec![path.to_vec()], MessageKind::Contents, text);
        }
        Ok((Version { mode, oid }, clean))
    }

    /// The three-way merge of the contents of two regular files, with the
    /// driver and marker size of the path's merge attributes.
    fn merge_content(
        &mut self,
        path: &[u8],
        b: Entry,
        o: Version,
        t: Version,
    ) -> Result<(ObjectId, bool), Error> {
        // A base of another type (a symbolic link) counts as empty.
        let mut base = match b {
            Some(b) if kind(b.mode) == Kind::Regular => self.read_blob(&b.oid)?,
            _ => Vec::new(),
        };
        let mut ours = self.read_blob(&o.oid)?;
        let mut theirs = self.read_blob(&t.oid)?;
        let (driver, attr_size) = match &self.opts.path_merge {
            Some(pm) => pm.driver(path, self.depth > 0),
            None => (Driver::Text, None),
        };
        if self.opts.renormalize {
            if let Some(pm) = &self.opts.path_merge {
                base = pm.renormalize(path, base);
                ours = pm.renormalize(path, ours);
                theirs = pm.renormalize(path, theirs);
                if ours == theirs || base == theirs {
                    return Ok((self.write(ObjectKind::Blob, &ours)?, true));
                }
                if base == ours {
                    return Ok((self.write(ObjectKind::Blob, &theirs)?, true));
                }
            }
        }
        let marker_size = match attr_size.unwrap_or(self.opts.marker_size) {
            0 => text::DEFAULT_MARKER_SIZE,
            n => n,
        } + 2 * self.depth
            + self.extra_marker;
        let favor = match driver {
            Driver::Union => Favor::Union,
            _ => self.opts.favor,
        };
        // A file merged at a path it was renamed to: the labels name the
        // path on each side.
        let with_path = |label: &[u8], p: &[u8]| [label, b":", p].concat();
        let [base_label, ours_label, theirs_label] = match self.notes.labels.get(path) {
            Some(p) if p[1] != p[2] => [
                with_path(self.labels.base, &p[0]),
                with_path(self.labels.ours, &p[1]),
                with_path(self.labels.theirs, &p[2]),
            ],
            _ => [
                self.labels.base.to_vec(),
                self.labels.ours.to_vec(),
                self.labels.theirs.to_vec(),
            ],
        };
        if let (Driver::Custom(name), Some(pm)) = (&driver, &self.opts.path_merge) {
            let input = DriverInput {
                path,
                base: &base,
                ours: &ours,
                theirs: &theirs,
                marker_size,
                base_label: &base_label,
                ours_label: &ours_label,
                theirs_label: &theirs_label,
            };
            let (data, clean) = pm.run(name, &input).map_err(Error::Driver)?;
            return Ok((self.write(ObjectKind::Blob, &data)?, clean));
        }
        let mopts = MergeOptions {
            style: self.opts.style,
            favor,
            marker_size,
            ours_label: &ours_label,
            base_label: &base_label,
            theirs_label: &theirs_label,
            diff: DiffOptions {
                algorithm: self.opts.algorithm,
                minimal: self.opts.minimal,
                whitespace: self.opts.whitespace,
                ..text::merge_diff_options()
            },
        };
        let merged = if driver == Driver::Binary {
            Err(text::BinaryContent)
        } else {
            text::merge(&base, &ours, &theirs, &mopts)
        };
        match merged {
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
        None => return Err(Error::MissingTree(*id)),
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
