// SPDX-License-Identifier: MIT

//! Rename and directory rename detection for the merge.
//!
//! Before the tree walk, renames are detected on each side relative to
//! the base. The walk then runs on rewritten trees: the base version of a
//! renamed file and the other side's version of it move to the new path,
//! so that the walk merges the three versions there like any other path.
//! What is about the renames themselves (rename/delete, rename/rename,
//! files moved by a directory rename) is either reported here or noted
//! per path for the walk ([`Notes`]).
//!
//! The rules come from `Documentation/technical/directory-rename-detection.adoc`,
//! `Documentation/merge-strategies.adoc`, `Documentation/config/merge.adoc`
//! and differential tests against the `git` binary:
//!
//! - Only files deleted on a side are rename sources, only files added on
//!   it destinations. Exact renames are found for all sources; the
//!   similarity search only uses sources the merge needs: those the other
//!   side did not keep unchanged, and those in a directory the side removed
//!   while the other side added something to it.
//! - A renamed file the other side changed is merged at the new path; the
//!   conflict markers then name the path on each side (`HEAD:new`).
//! - Renamed on one side, deleted on the other: rename/delete, the renamed
//!   version stays. Renamed to two different paths: rename/rename, the
//!   merged contents at both new paths (markers one character longer) and
//!   the base version at the old path. A rename onto a path the other
//!   side also added (or renamed something to) becomes an add/add of the
//!   renamed version (merged with the other side's changes) and that file.
//! - A directory counts as renamed on a side when the side removed it and
//!   the renames out of it agree on a target: the one with the most files
//!   wins, a tie is a "directory rename split". A rename `a/b/c/f` to
//!   `a/x/c/f` also counts for `a/b` to `a/x`. Paths the other side added
//!   to such a directory (by adding or renaming files) move along, to the
//!   deepest renamed directory, unless a file or directory of that side is
//!   in the way or several paths would end up at the same place.

use std::collections::{BTreeMap, HashMap, HashSet};

use rgit_diff::diffcore::{self, Detect, FilePair, Side};

use super::{Case, Engine, Entry, Kind, classify, kind, read_tree};
use crate::{DirectoryRenames, Error, MessageKind, ObjectId, ObjectKind, Version, messages};
use rgit_object::{EntryMode, Tree, TreeEntry};

/// What the tree walk needs to know about paths that renames touched.
#[derive(Debug, Default)]
pub(super) struct Notes {
    /// The paths of the base, ours and theirs of a file merged at a path
    /// it was renamed to; they go into the conflict marker labels.
    pub labels: HashMap<Vec<u8>, [Vec<u8>; 3]>,
    /// Renamed on one side and deleted on the other: a conflict even when
    /// the renamed file is unchanged.
    pub rename_delete: HashSet<Vec<u8>>,
    /// Moved by a directory rename with `merge.directoryRenames=conflict`.
    pub location: HashSet<Vec<u8>>,
    /// Renamed on one side and moved back by the other side's directory
    /// rename, which deleted the file: a modify/delete conflict even when
    /// the file is unchanged.
    pub modify_delete: HashSet<Vec<u8>>,
    /// The directories that contain a noted path.
    dirs: HashSet<Vec<u8>>,
}

impl Notes {
    /// Whether the walk must look at `path` (without a trailing slash)
    /// itself instead of taking it from one side as a whole.
    pub fn touches(&self, path: &[u8]) -> bool {
        self.dirs.contains(path)
            || self.rename_delete.contains(path)
            || self.location.contains(path)
            || self.modify_delete.contains(path)
    }

    fn mark(&mut self, path: &[u8]) {
        let mut p = path;
        while let Some(i) = p.iter().rposition(|&c| c == b'/') {
            p = &p[..i];
            if !self.dirs.insert(p.to_vec()) {
                break;
            }
        }
    }
}

/// Files deleted and added on one side.
#[derive(Debug, Default)]
struct Changes {
    deleted: Vec<(Vec<u8>, Version)>,
    added: Vec<(Vec<u8>, Version)>,
}

/// The parent directory of a path ("" at the top).
fn dirname(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&c| c == b'/') {
        Some(i) => &path[..i],
        None => b"",
    }
}

fn basename(path: &[u8]) -> &[u8] {
    match path.iter().rposition(|&c| c == b'/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// The directories above `path`, deepest first (not the top level).
fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut p = path;
    std::iter::from_fn(move || {
        let i = p.iter().rposition(|&c| c == b'/')?;
        p = &p[..i];
        Some(p)
    })
}

/// The directory rename of `side` (0 ours, 1 theirs) that applies to a
/// path the other side added: the deepest renamed directory above it that
/// was not split, and the new path (`None` when the other side renamed
/// the target directory away).
fn dir_rename_target<'p>(
    dirs: &[HashMap<Vec<u8>, Option<Vec<u8>>>; 2],
    side: usize,
    path: &'p [u8],
) -> Option<(&'p [u8], Option<Vec<u8>>)> {
    let (dir, t) = ancestors(path).find_map(|d| match dirs[side].get(d) {
        Some(Some(t)) => Some((d, t)),
        _ => None,
    })?;
    // A split rename of the target does not count.
    if matches!(dirs[1 - side].get(t), Some(Some(_))) {
        return Some((dir, None));
    }
    Some((dir, Some(join_path(t, &path[dir.len() + 1..]))))
}

fn join_path(dir: &[u8], rest: &[u8]) -> Vec<u8> {
    if dir.is_empty() {
        rest.to_vec()
    } else {
        [dir, b"/", rest].concat()
    }
}

const TYPE_MASK: u32 = 0o170000;

/// The state of the rename pass: the three input trees and the edits to
/// them (`None` removes a path).
struct Pass {
    roots: [Option<ObjectId>; 3],
    edits: [BTreeMap<Vec<u8>, Option<Version>>; 3],
    /// Paths moved by a directory rename: new path to the path the side
    /// gave it (conflict marker labels use that one).
    moved: HashMap<Vec<u8>, Vec<u8>>,
    cache: HashMap<ObjectId, Vec<(Vec<u8>, Version)>>,
}

impl Engine<'_> {
    /// Whether directory renames are detected. The merges of merge bases
    /// skip them (t6423 13e; git behaves so with `true` as well).
    fn dir_renames_on(&self) -> bool {
        self.opts.directory_renames != DirectoryRenames::False && self.depth == 0
    }

    /// Finds the renames of both sides and returns the trees to merge
    /// instead of `base`, `ours` and `theirs`, or `None` when nothing was
    /// renamed.
    pub(super) fn prepare_renames(
        &mut self,
        base: &ObjectId,
        ours: &ObjectId,
        theirs: &ObjectId,
    ) -> Result<Option<[ObjectId; 3]>, Error> {
        let mut changes = [Changes::default(), Changes::default()];
        for (side, tree) in [ours, theirs].into_iter().enumerate() {
            collect_changes(
                self,
                &mut Vec::new(),
                Some(*base),
                Some(*tree),
                &mut changes[side],
            )?;
        }
        if changes
            .iter()
            .all(|c| c.deleted.is_empty() || c.added.is_empty())
        {
            return Ok(None);
        }
        let mut pass = Pass {
            roots: [Some(*base), Some(*ours), Some(*theirs)],
            edits: Default::default(),
            moved: HashMap::new(),
            cache: HashMap::new(),
        };
        // Directories each side added paths to directly: only they matter
        // for directory renames of the other side.
        let mut added_dirs: [HashSet<Vec<u8>>; 2] = Default::default();
        if self.dir_renames_on() {
            for (side, c) in changes.iter().enumerate() {
                for (p, _) in &c.added {
                    let d = dirname(p);
                    if !d.is_empty() {
                        added_dirs[side].insert(d.to_vec());
                    }
                }
            }
        }
        let mut renames: [BTreeMap<Vec<u8>, Vec<u8>>; 2] = Default::default();
        for side in 0..2 {
            renames[side] =
                self.detect_renames(&mut pass, side, &changes[side], &added_dirs[1 - side])?;
        }
        if renames.iter().all(BTreeMap::is_empty) {
            return Ok(None);
        }
        if self.dir_renames_on() {
            let mut dirs: [HashMap<Vec<u8>, Option<Vec<u8>>>; 2] = Default::default();
            for side in 0..2 {
                dirs[side] =
                    self.renamed_dirs(&mut pass, side, &renames[side], &added_dirs[1 - side])?;
            }
            self.dir_renames_found = dirs.iter().any(|d| !d.is_empty());
            // Where each side's added paths would go by the other side's
            // directory renames. A path that the other side's own file
            // moves to stays (the two meet in an add/add).
            let mut wanted: [HashSet<Vec<u8>>; 2] = Default::default();
            for side in 0..2 {
                wanted[1 - side] = changes[1 - side]
                    .added
                    .iter()
                    .filter_map(|(p, _)| match dir_rename_target(&dirs, side, p) {
                        Some((_, Some(new))) => Some(new),
                        _ => None,
                    })
                    .collect();
            }
            let mut moves: [BTreeMap<Vec<u8>, Vec<u8>>; 2] = Default::default();
            // Our paths moved by their directory renames are reported
            // first.
            for side in [1, 0] {
                moves[1 - side] = self.directory_renames(
                    &mut pass,
                    side,
                    &dirs,
                    &renames,
                    &changes[1 - side],
                    &wanted[side],
                )?;
            }
            for (other, m) in moves.iter().enumerate() {
                for (from, to) in m {
                    let v = pass.get(self, other + 1, from)?;
                    pass.edits[other + 1].insert(from.clone(), None);
                    pass.edits[other + 1].insert(to.clone(), v);
                    for d in renames[other].values_mut() {
                        if d == from {
                            d.clone_from(to);
                        }
                    }
                }
            }
            // A rename that a directory rename took back is none. If the
            // side that renamed the directory deleted the file, git still
            // reports a modify/delete conflict.
            for (other, r) in renames.iter_mut().enumerate() {
                let back: Vec<Vec<u8>> = r
                    .iter()
                    .filter(|(s, d)| s == d)
                    .map(|(s, _)| s.clone())
                    .collect();
                for s in back {
                    r.remove(&s);
                    if pass.get(self, 2 - other, &s)?.is_none() {
                        self.notes.modify_delete.insert(s.clone());
                        self.notes.mark(&s);
                    }
                }
            }
        }
        self.pair_renames(&mut pass, &renames)?;
        let mut out = [*base, *ours, *theirs];
        for (i, slot) in out.iter_mut().enumerate() {
            let edits = std::mem::take(&mut pass.edits[i]);
            if edits.is_empty() {
                continue;
            }
            *slot = match self.edit_tree(pass.roots[i], &edits, &mut pass.cache)? {
                Some(id) => id,
                None => self.write(ObjectKind::Tree, &[])?,
            };
        }
        Ok(Some(out))
    }

    /// The renames of one side (0 ours, 1 theirs): source to destination.
    fn detect_renames(
        &mut self,
        pass: &mut Pass,
        side: usize,
        changes: &Changes,
        other_added_dirs: &HashSet<Vec<u8>>,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, Error> {
        let mut out = BTreeMap::new();
        // Empty files are never rename sources.
        let empty = self.objects.hash_kind().empty_blob();
        let sources: Vec<&(Vec<u8>, Version)> = changes
            .deleted
            .iter()
            .filter(|(_, v)| v.oid != empty)
            .collect();
        let mut dests: Vec<&(Vec<u8>, Version)> = changes.added.iter().collect();
        if sources.is_empty() || dests.is_empty() {
            return Ok(out);
        }
        // The sources the merge needs. Without any, nothing is detected;
        // otherwise exact renames are found for all sources and the
        // similarity search uses the needed ones.
        let other = 2 - side;
        let mut relevant = Vec::with_capacity(sources.len());
        for (path, v) in sources.iter().copied() {
            let theirs = pass.get(self, other, path)?;
            let mut needed = theirs != Some(*v);
            if !needed {
                needed = pass.relevant_dir(self, side, dirname(path), other_added_dirs)?;
            }
            relevant.push(needed);
        }
        if !relevant.contains(&true) {
            return Ok(out);
        }
        let mut sources: Vec<(&(Vec<u8>, Version), bool)> =
            sources.into_iter().zip(relevant).collect();
        sources.sort_by(|a, b| a.0.0.cmp(&b.0.0));
        dests.sort_by(|a, b| a.0.cmp(&b.0));

        // Exact renames: the same blob and type. Among several sources one
        // with the same file name wins, else the first.
        let mut by_oid: HashMap<ObjectId, Vec<usize>> = HashMap::new();
        for (k, ((_, v), _)) in sources.iter().enumerate() {
            by_oid.entry(v.oid).or_default().push(k);
        }
        let mut used = vec![false; sources.len()];
        let mut matched = vec![false; dests.len()];
        for (di, (dpath, dv)) in dests.iter().enumerate() {
            let Some(cands) = by_oid.get(&dv.oid) else {
                continue;
            };
            let ok: Vec<usize> = cands
                .iter()
                .copied()
                .filter(|&k| !used[k] && sources[k].0.1.mode & TYPE_MASK == dv.mode & TYPE_MASK)
                .collect();
            let pick = ok
                .iter()
                .copied()
                .find(|&k| basename(&sources[k].0.0) == basename(dpath))
                .or_else(|| ok.first().copied());
            if let Some(k) = pick {
                used[k] = true;
                matched[di] = true;
                out.insert(sources[k].0.0.clone(), dpath.clone());
            }
        }

        // The similarity search.
        let mut pairs: Vec<FilePair<()>> = Vec::new();
        for (k, ((path, v), needed)) in sources.iter().enumerate() {
            if *needed && !used[k] && kind(v.mode) != Kind::Gitlink {
                pairs.push(FilePair::new(Some(side_of(path, v)), None));
            }
        }
        if pairs.is_empty() {
            return Ok(out);
        }
        for (di, (path, v)) in dests.iter().enumerate() {
            if !matched[di] && kind(v.mode) != Kind::Gitlink {
                pairs.push(FilePair::new(None, Some(side_of(path, v))));
            }
        }
        pairs.sort_by(|a, b| pair_path(a).cmp(pair_path(b)));
        let dopts = diffcore::Options {
            detect: Detect::Renames,
            rename_score: self.opts.rename_score,
            rename_limit: self.opts.rename_limit,
            rename_empty: false,
            ..diffcore::Options::default()
        };
        let contents = |s: &Side<()>| self.read_blob(&s.oid).unwrap_or_default();
        let (found, report) = diffcore::run(pairs, &dopts, &contents);
        if let Some(n) = report.rename_limit_needed {
            self.rename_limit_needed = Some(self.rename_limit_needed.map_or(n, |m| m.max(n)));
        }
        for p in found {
            if p.status == diffcore::Status::Renamed
                && let (Some(a), Some(b)) = (p.one, p.two)
            {
                out.insert(a.path, b.path);
            }
        }
        Ok(out)
    }

    /// The directories `side` (0 ours, 1 theirs) renamed, among those
    /// where that matters ([`Pass::relevant_dir`]): old to new, `None`
    /// when split.
    fn renamed_dirs(
        &mut self,
        pass: &mut Pass,
        side: usize,
        renames: &BTreeMap<Vec<u8>, Vec<u8>>,
        other_added_dirs: &HashSet<Vec<u8>>,
    ) -> Result<HashMap<Vec<u8>, Option<Vec<u8>>>, Error> {
        // Votes per removed directory for its new place.
        let mut votes: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, usize>> = BTreeMap::new();
        for (src, dst) in renames {
            let (mut s, mut d) = (dirname(src), dirname(dst));
            while !s.is_empty() && s != d {
                if pass.relevant_dir(self, side, s, other_added_dirs)? {
                    *votes
                        .entry(s.to_vec())
                        .or_default()
                        .entry(d.to_vec())
                        .or_default() += 1;
                }
                if d.is_empty() || basename(s) != basename(d) {
                    break;
                }
                (s, d) = (dirname(s), dirname(d));
            }
        }
        // `None`: split between several targets.
        let mut renamed: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
        for (dir, targets) in votes {
            let best = targets.values().copied().max().unwrap_or(0);
            let mut winners = targets.into_iter().filter(|&(_, n)| n == best);
            let first = winners.next().map(|(t, _)| t);
            if winners.next().is_some() {
                self.clean = false;
                let text = messages::directory_rename_split(&dir);
                self.message(vec![dir.clone()], MessageKind::DirectoryRenameSplit, text);
                renamed.insert(dir, None);
            } else {
                renamed.insert(dir, first);
            }
        }
        Ok(renamed)
    }

    /// The directory renames of `side` (0 ours, 1 theirs), applied to the
    /// paths the other side added: returns where they move.
    fn directory_renames(
        &mut self,
        pass: &mut Pass,
        side: usize,
        dirs: &[HashMap<Vec<u8>, Option<Vec<u8>>>; 2],
        renames: &[BTreeMap<Vec<u8>, Vec<u8>>; 2],
        other_changes: &Changes,
        own_targets: &HashSet<Vec<u8>>,
    ) -> Result<BTreeMap<Vec<u8>, Vec<u8>>, Error> {
        let mut moves = BTreeMap::new();
        let renamed = &dirs[side];
        if renamed.is_empty() {
            return Ok(moves);
        }
        let other = 1 - side;
        let (this_label, other_label) = if side == 0 {
            (self.labels.ours, self.labels.theirs)
        } else {
            (self.labels.theirs, self.labels.ours)
        };
        let renamed_from: HashMap<&[u8], &[u8]> = renames[other]
            .iter()
            .map(|(s, d)| (d.as_slice(), s.as_slice()))
            .collect();
        let mut targets: BTreeMap<Vec<u8>, Vec<Vec<u8>>> = BTreeMap::new();
        let mut added: Vec<&Vec<u8>> = other_changes.added.iter().map(|(p, _)| p).collect();
        added.sort();
        for path in added {
            if own_targets.contains(path) {
                continue;
            }
            match dir_rename_target(dirs, side, path) {
                None => {}
                Some((dir, None)) => {
                    // The other side renamed the target away.
                    let t = renamed[dir].clone().unwrap_or_default();
                    let text = messages::directory_rename_skipped(dir, &t, path);
                    self.message(
                        vec![dir.to_vec(), path.clone(), t],
                        MessageKind::DirectoryRenameSkipped,
                        text,
                    );
                }
                Some((_, Some(new))) => targets.entry(new).or_default().push(path.clone()),
            }
        }
        for (new, paths) in targets {
            let refs: Vec<&[u8]> = paths.iter().map(Vec::as_slice).collect();
            let mut msg_paths = vec![new.clone()];
            msg_paths.extend(paths.iter().cloned());
            // In the way: a file or directory of the other side, or of the
            // base unless the path is the rename of that very file.
            let back =
                paths.len() == 1 && renamed_from.get(paths[0].as_slice()) == Some(&new.as_slice());
            if pass.get_any(self, other + 1, &new)? || (!back && pass.get_any(self, 0, &new)?) {
                self.clean = false;
                let text = messages::file_in_the_way(&new, &refs);
                self.message(msg_paths, MessageKind::FileInTheWay, text);
                continue;
            }
            if paths.len() > 1 {
                self.clean = false;
                let text = messages::directory_rename_collision(&new, &refs);
                self.message(msg_paths, MessageKind::DirectoryRenameCollision, text);
                continue;
            }
            let path = &paths[0];
            let from = renamed_from.get(path.as_slice()).copied();
            if self.opts.directory_renames == DirectoryRenames::Conflict {
                let text = messages::file_location(path, from, other_label, this_label, &new);
                self.message(msg_paths, MessageKind::DirectoryRenameSuggested, text);
                self.notes.location.insert(new.clone());
                self.notes.mark(&new);
            } else {
                let text = messages::path_updated(path, from, other_label, this_label, &new);
                self.message(msg_paths, MessageKind::PathUpdated, text);
            }
            // Both sides may move a file to the same place (an add/add):
            // each side's label keeps the path that side gave its file.
            match self.notes.labels.get_mut(&new) {
                Some(labels) => labels[other + 1].clone_from(path),
                None => {
                    let mut labels = [path.clone(), path.clone(), path.clone()];
                    labels[0].clone_from(&new);
                    labels[side + 1].clone_from(&new);
                    self.notes.labels.insert(new.clone(), labels);
                }
            }
            pass.moved.insert(new.clone(), path.clone());
            moves.insert(path.clone(), new);
        }
        Ok(moves)
    }

    /// Rewrites the trees for the renames of both sides and reports the
    /// conflicts that are about renames.
    fn pair_renames(
        &mut self,
        pass: &mut Pass,
        renames: &[BTreeMap<Vec<u8>, Vec<u8>>; 2],
    ) -> Result<(), Error> {
        let mut sources: Vec<&Vec<u8>> = renames[0].keys().chain(renames[1].keys()).collect();
        sources.sort();
        sources.dedup();
        // Whether the other side has its own file at a rename target,
        // before any rename moves something there.
        let mut collides = HashMap::new();
        for (side, r) in renames.iter().enumerate() {
            for (src, dst) in r {
                let same = renames[1 - side].get(src) == Some(dst);
                let other_has = pass.get(self, 2 - side, dst)?.is_some();
                collides.insert((side, src.clone()), other_has && !same && src != dst);
            }
        }
        for src in sources {
            let base = pass.get(self, 0, src)?;
            match (renames[0].get(src), renames[1].get(src)) {
                (Some(d1), Some(d2)) if d1 == d2 => {
                    pass.edits[0].insert(src.clone(), None);
                    pass.edits[0].insert(d1.clone(), base);
                }
                (Some(d1), Some(d2)) => self.rename_rename(pass, src, d1, d2, base)?,
                (Some(d), None) => {
                    let collide = collides[&(0, src.clone())];
                    self.one_sided(pass, 0, src, d, base, collide)?;
                }
                (None, Some(d)) => {
                    let collide = collides[&(1, src.clone())];
                    self.one_sided(pass, 1, src, d, base, collide)?;
                }
                (None, None) => {}
            }
        }
        Ok(())
    }

    /// A file renamed on one side only (`side` 0 ours, 1 theirs).
    fn one_sided(
        &mut self,
        pass: &mut Pass,
        side: usize,
        src: &[u8],
        dst: &[u8],
        base: Entry,
        collide: bool,
    ) -> Result<(), Error> {
        let (x, y) = (side + 1, 2 - side);
        // A different type on the other side is a new file there.
        let other = pass
            .get(self, y, src)?
            .filter(|v| base.is_none_or(|b| kind(b.mode) == kind(v.mode)));
        pass.edits[0].insert(src.to_vec(), None);
        let Some(other) = other else {
            // Deleted on the other side.
            let (renamed_in, deleted_in) = if side == 0 {
                (self.labels.ours, self.labels.theirs)
            } else {
                (self.labels.theirs, self.labels.ours)
            };
            let text = messages::rename_delete(src, dst, renamed_in, deleted_in);
            self.message(
                vec![dst.to_vec(), src.to_vec()],
                MessageKind::RenameDelete,
                text,
            );
            if !collide {
                pass.edits[0].insert(dst.to_vec(), base);
                self.notes.rename_delete.insert(dst.to_vec());
                self.notes.mark(dst);
            }
            return Ok(());
        };
        pass.edits[y].insert(src.to_vec(), None);
        let mut paths = [src.to_vec(), src.to_vec(), src.to_vec()];
        paths[x] = pass.moved.get(dst).map_or(dst, Vec::as_slice).to_vec();
        let before = self.notes.labels.insert(dst.to_vec(), paths);
        if !collide {
            pass.edits[0].insert(dst.to_vec(), base);
            pass.edits[y].insert(dst.to_vec(), Some(other));
            return Ok(());
        }
        // The other side has its own file at `dst`: the renamed version,
        // merged with the other side's changes, meets it there.
        let mine = pass.get(self, x, dst)?;
        let (o, t) = if side == 0 {
            (mine, Some(other))
        } else {
            (Some(other), mine)
        };
        let merged = match classify(base, o, t) {
            Case::Take(v) => v,
            Case::Both(Kind::Regular) => {
                let (o, t) = (o.expect("both sides"), t.expect("both sides"));
                // Reported at the old path; markers one longer, as they
                // may end up inside the add/add conflict markers.
                let labels = self.notes.labels.get(dst).cloned();
                let saved = labels.and_then(|l| self.notes.labels.insert(src.to_vec(), l));
                self.extra_marker = 1;
                self.quiet_conflict = true;
                let r = self.merge_regular(src, base, o, t);
                self.extra_marker = 0;
                self.quiet_conflict = false;
                match saved {
                    Some(l) => self.notes.labels.insert(src.to_vec(), l),
                    None => self.notes.labels.remove(src),
                };
                let (v, clean) = r?;
                if !clean {
                    let text = messages::rename_collision(src, dst);
                    self.message(
                        vec![dst.to_vec(), src.to_vec()],
                        MessageKind::RenameCollision,
                        text,
                    );
                }
                Some(v)
            }
            _ => mine,
        };
        match before {
            Some(l) => self.notes.labels.insert(dst.to_vec(), l),
            None => self.notes.labels.remove(dst),
        };
        pass.edits[x].insert(dst.to_vec(), merged);
        Ok(())
    }

    /// A file renamed to different paths on the two sides.
    fn rename_rename(
        &mut self,
        pass: &mut Pass,
        src: &[u8],
        d1: &[u8],
        d2: &[u8],
        base: Entry,
    ) -> Result<(), Error> {
        let o = pass.get(self, 1, d1)?;
        let t = pass.get(self, 2, d2)?;
        let (mut mo, mut mt) = (o, t);
        if let (Some(ov), Some(tv)) = (o, t)
            && kind(ov.mode) == Kind::Regular
            && kind(tv.mode) == Kind::Regular
            && base.is_some_and(|b| kind(b.mode) == Kind::Regular)
        {
            self.notes
                .labels
                .insert(src.to_vec(), [src.to_vec(), d1.to_vec(), d2.to_vec()]);
            self.extra_marker = 1;
            self.quiet_conflict = true;
            self.binary_conflict = false;
            let r = self.merge_regular(src, base, ov, tv);
            self.extra_marker = 0;
            self.quiet_conflict = false;
            self.notes.labels.remove(src);
            let merged = r?.0;
            // Unmerged binary files keep each side's version.
            if !std::mem::take(&mut self.binary_conflict) {
                (mo, mt) = (Some(merged), Some(merged));
            }
        }
        let text = messages::rename_rename(src, d1, self.labels.ours, d2, self.labels.theirs);
        self.message(
            vec![src.to_vec(), d1.to_vec(), d2.to_vec()],
            MessageKind::RenameRename,
            text,
        );
        self.conflict(src.to_vec(), [base, None, None]);
        self.conflict(d1.to_vec(), [None, mo, None]);
        self.conflict(d2.to_vec(), [None, None, mt]);
        pass.edits[0].insert(src.to_vec(), None);
        pass.edits[1].insert(d1.to_vec(), mo);
        pass.edits[2].insert(d2.to_vec(), mt);
        Ok(())
    }

    /// Applies `edits` (paths below `root`) and writes the changed trees.
    fn edit_tree(
        &self,
        root: Option<ObjectId>,
        edits: &BTreeMap<Vec<u8>, Option<Version>>,
        cache: &mut HashMap<ObjectId, Vec<(Vec<u8>, Version)>>,
    ) -> Result<Option<ObjectId>, Error> {
        // By name and whether it is a directory: a file may take the name
        // of a directory that is still there (the tree walk sorts it out).
        let mut entries: BTreeMap<(Vec<u8>, bool), Version> = match root {
            Some(id) => cached_tree(self, cache, &id)?
                .into_iter()
                .map(|(n, v)| ((n, kind(v.mode) == Kind::Tree), v))
                .collect(),
            None => BTreeMap::new(),
        };
        let mut sub: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, Option<Version>>> = BTreeMap::new();
        for (path, v) in edits {
            match path.iter().position(|&c| c == b'/') {
                Some(i) => {
                    sub.entry(path[..i].to_vec())
                        .or_default()
                        .insert(path[i + 1..].to_vec(), *v);
                }
                None => match v {
                    Some(v) => {
                        entries.insert((path.clone(), false), *v);
                    }
                    None => {
                        entries.remove(&(path.clone(), false));
                    }
                },
            }
        }
        for (name, e) in sub {
            let key = (name, true);
            let old = entries.get(&key).map(|v| v.oid);
            match self.edit_tree(old, &e, cache)? {
                Some(id) => {
                    let mode = super::MODE_TREE;
                    entries.insert(key, Version { mode, oid: id });
                }
                None => {
                    entries.remove(&key);
                }
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let mut tree = Tree {
            entries: entries
                .into_iter()
                .map(|((name, _), v)| TreeEntry::new(EntryMode::from_bits(v.mode), name, v.oid))
                .collect(),
        };
        tree.sort();
        Ok(Some(self.write(ObjectKind::Tree, &tree.serialize())?))
    }
}

impl Pass {
    /// The entry at `path` in tree `which` (0 base, 1 ours, 2 theirs) as
    /// edited so far; directories count as absent.
    fn get(&mut self, e: &Engine<'_>, which: usize, path: &[u8]) -> Result<Entry, Error> {
        if let Some(v) = self.edits[which].get(path) {
            return Ok(*v);
        }
        Ok(self
            .lookup(e, which, path)?
            .filter(|v| kind(v.mode) != Kind::Tree))
    }

    /// Whether anything (file or directory) is at `path` in tree `which`.
    fn get_any(&mut self, e: &Engine<'_>, which: usize, path: &[u8]) -> Result<bool, Error> {
        if let Some(v) = self.edits[which].get(path) {
            return Ok(v.is_some());
        }
        Ok(self.lookup(e, which, path)?.is_some())
    }

    /// Whether a rename of `dir` by `side` (0 ours, 1 theirs) can matter:
    /// the side removed it, and the other side added paths to it or to a
    /// directory above it that the side removed too.
    fn relevant_dir(
        &mut self,
        e: &Engine<'_>,
        side: usize,
        dir: &[u8],
        other_added: &HashSet<Vec<u8>>,
    ) -> Result<bool, Error> {
        let mut d = dir;
        while !d.is_empty() {
            if self.is_dir(e, side + 1, d)? {
                return Ok(false);
            }
            if other_added.contains(d) {
                return Ok(true);
            }
            d = dirname(d);
        }
        Ok(false)
    }

    /// Whether `path` is a directory in the unedited tree `which`.
    fn is_dir(&mut self, e: &Engine<'_>, which: usize, path: &[u8]) -> Result<bool, Error> {
        Ok(self
            .lookup(e, which, path)?
            .is_some_and(|v| kind(v.mode) == Kind::Tree))
    }

    fn lookup(&mut self, e: &Engine<'_>, which: usize, path: &[u8]) -> Result<Entry, Error> {
        let Some(mut tree) = self.roots[which] else {
            return Ok(None);
        };
        let mut parts = path.split(|&c| c == b'/').peekable();
        while let Some(name) = parts.next() {
            let entries = cached_tree(e, &mut self.cache, &tree)?;
            let Some((_, v)) = entries.iter().find(|(n, _)| n.as_slice() == name) else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(*v));
            }
            if kind(v.mode) != Kind::Tree {
                return Ok(None);
            }
            tree = v.oid;
        }
        Ok(None)
    }
}

fn cached_tree(
    e: &Engine<'_>,
    cache: &mut HashMap<ObjectId, Vec<(Vec<u8>, Version)>>,
    id: &ObjectId,
) -> Result<Vec<(Vec<u8>, Version)>, Error> {
    if let Some(v) = cache.get(id) {
        return Ok(v.clone());
    }
    let v = read_tree(e.objects, id)?;
    cache.insert(*id, v.clone());
    Ok(v)
}

fn side_of(path: &[u8], v: &Version) -> Side<()> {
    Side {
        path: path.to_vec(),
        mode: v.mode,
        oid: v.oid,
        extra: (),
    }
}

fn pair_path(p: &FilePair<()>) -> &[u8] {
    p.one.as_ref().or(p.two.as_ref()).map_or(&[], |s| &s.path)
}

/// The files `side` deleted and added relative to `base`, skipping
/// subtrees that did not change.
fn collect_changes(
    e: &Engine<'_>,
    prefix: &mut Vec<u8>,
    base: Option<ObjectId>,
    side: Option<ObjectId>,
    out: &mut Changes,
) -> Result<(), Error> {
    let mut names: BTreeMap<Vec<u8>, [Entry; 2]> = BTreeMap::new();
    for (i, tree) in [base, side].into_iter().enumerate() {
        if let Some(id) = tree {
            for (name, v) in read_tree(e.objects, &id)? {
                names.entry(name).or_default()[i] = Some(v);
            }
        }
    }
    let split = |x: Entry| match x {
        Some(v) if kind(v.mode) == Kind::Tree => (None, Some(v.oid)),
        other => (other, None),
    };
    for (name, [b, s]) in names {
        if b == s {
            continue;
        }
        let (bf, bd) = split(b);
        let (sf, sd) = split(s);
        let path = [prefix.as_slice(), &name].concat();
        match (bf, sf) {
            (Some(v), None) => out.deleted.push((path.clone(), v)),
            (None, Some(v)) => out.added.push((path.clone(), v)),
            // A file that became a symbolic link (or the other way) is
            // gone as far as renames are concerned.
            (Some(b), Some(s)) if kind(b.mode) != kind(s.mode) => {
                out.deleted.push((path.clone(), b));
                out.added.push((path.clone(), s));
            }
            _ => {}
        }
        if bd != sd {
            let len = prefix.len();
            prefix.extend_from_slice(&path[len..]);
            prefix.push(b'/');
            let r = collect_changes(e, prefix, bd, sd, out);
            prefix.truncate(len);
            r?;
        }
    }
    Ok(())
}
