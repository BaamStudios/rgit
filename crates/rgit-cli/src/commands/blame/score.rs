// SPDX-License-Identifier: MIT
//! The blame computation: which commit last changed each line.
//!
//! Every line range of the final file starts out suspecting the final
//! version (a commit, or the work tree as a pseudo commit). Commits are
//! visited newest first (by committer date). For a commit, the file is
//! looked up in each parent (same path, else a rename found by rename
//! detection). When a parent has the very same blob, everything passes to
//! that parent. Otherwise the parent's version is diffed with the commit's
//! version, parent by parent: lines outside the changed regions pass to the
//! parent, the rest stays with the commit, which is then blamed for them.
//! Commits outside the given revision range, older than `--since`, and
//! (without `--root`) root commits are boundaries: they take all lines that
//! reach them.
//!
//! `-M` and `-C` add passes over the lines still left with the commit: each
//! such range is diffed against the whole parent version of the file (`-M`)
//! or against other files of the parent (`-C`); the common block with the
//! most alphanumeric characters, if it reaches the threshold, passes to the
//! parent and the rest of the range is tried again.

use std::cell::{OnceCell, RefCell};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::ops::Range;
use std::rc::Rc;

use rgit_diff::DiffOptions;
use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, TreeIter};

use crate::repository::Repo;

/// The contents of one version of the file, split into lines.
pub struct Blob {
    pub data: Vec<u8>,
    /// Byte ranges of the lines (with their newline).
    bounds: Vec<(usize, usize)>,
    /// The interned ids of the lines (see [`Scoreboard::ids`]).
    ids: OnceCell<Vec<u32>>,
}

impl Blob {
    pub fn new(data: Vec<u8>) -> Blob {
        let mut bounds = Vec::with_capacity(data.len() / 32 + 1);
        let mut start = 0;
        while let Some(p) = bstr::ByteSlice::find_byte(&data[start..], b'\n') {
            bounds.push((start, start + p + 1));
            start += p + 1;
        }
        if start < data.len() {
            bounds.push((start, data.len()));
        }
        Blob {
            data,
            bounds,
            ids: OnceCell::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.bounds.len()
    }

    pub fn line(&self, i: usize) -> &[u8] {
        let (a, b) = self.bounds[i];
        &self.data[a..b]
    }

    pub fn lines(&self) -> Vec<&[u8]> {
        self.bounds.iter().map(|&(a, b)| &self.data[a..b]).collect()
    }

    /// The bytes of lines `r`.
    fn bytes(&self, r: &Range<usize>) -> &[u8] {
        if r.is_empty() {
            return &[];
        }
        &self.data[self.bounds[r.start].0..self.bounds[r.end - 1].1]
    }

    /// The number of lines of `r` that start within its first `len` bytes.
    fn lines_within(&self, r: &Range<usize>, len: usize) -> usize {
        if r.is_empty() {
            return 0;
        }
        let base = self.bounds[r.start].0;
        self.bounds[r.clone()].partition_point(|&(s, _)| s - base < len)
    }

    /// The number of alphanumeric characters in lines `r` (the score of a
    /// moved or copied block).
    fn score(&self, r: Range<usize>) -> usize {
        self.bytes(&r)
            .iter()
            .filter(|b| b.is_ascii_alphanumeric())
            .count()
    }
}

/// A commit taking part in the blame.
pub struct Node {
    /// Null for the pseudo commit of the work tree or `--contents`.
    pub id: ObjectId,
    pub commit: Option<Rc<Commit>>,
    parents: Option<Vec<ObjectId>>,
    tree: Option<ObjectId>,
    /// Committer time.
    pub time: i64,
    /// Reachable from an excluded revision.
    uninteresting: bool,
    /// Lines were blamed on it as a boundary commit.
    pub boundary: bool,
    queued: bool,
    /// The path list of the pseudo commit (for rename detection).
    fake_paths: Option<Rc<Flat>>,
}

/// The file at one path in one commit.
pub struct Origin {
    pub commit: usize,
    pub path: Vec<u8>,
    pub blob: ObjectId,
    content: OnceCell<Rc<Blob>>,
    /// The version in the first parent that had the file (`previous` in
    /// the porcelain output).
    pub previous: Option<usize>,
}

/// Lines `lno..lno + num` of the final file, which are lines
/// `s_lno..s_lno + num` of the suspect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub lno: usize,
    pub num: usize,
    pub suspect: usize,
    pub s_lno: usize,
}

impl Entry {
    /// Lines `from..to` of the suspect (within this entry).
    fn piece(&self, from: usize, to: usize) -> Entry {
        Entry {
            lno: self.lno + (from - self.s_lno),
            num: to - from,
            suspect: self.suspect,
            s_lno: from,
        }
    }
}

/// Path → (mode, blob) of a whole tree.
type Flat = std::collections::BTreeMap<Vec<u8>, (u32, ObjectId)>;

/// Settings of the computation.
#[derive(Clone, Default)]
pub struct Settings {
    pub first_parent: bool,
    pub show_root: bool,
    /// `--since`: commits older than this are boundaries.
    pub since: Option<i64>,
    pub diff: DiffOptions,
    /// `-w`.
    pub ignore_whitespace: bool,
    /// `-M`: the minimum score of a block moved within the file.
    pub moves: Option<usize>,
    /// How often `-C` was given (0 to 3).
    pub copies: u8,
    /// The minimum score of a block copied from another file.
    pub copy_score: usize,
}

/// A block found by [`Scoreboard::best_match`]: `len` lines at `t` of the
/// suspect come from line `p` of origin `origin`.
#[derive(Clone, Copy)]
struct Match {
    score: usize,
    origin: usize,
    t: usize,
    p: usize,
    len: usize,
}

pub struct Scoreboard<'r> {
    repo: &'r Repo,
    pub nodes: Vec<Node>,
    ids: HashMap<ObjectId, usize>,
    pub origins: Vec<Origin>,
    origin_ids: HashMap<(usize, Vec<u8>), usize>,
    pending: HashMap<usize, Vec<Entry>>,
    queue: BinaryHeap<(i64, Reverse<u64>, usize)>,
    seq: u64,
    /// Entries whose suspect is final.
    pub done: Vec<Entry>,
    /// Where each batch of entries finished together starts in `done`
    /// (the order of `--incremental`).
    pub batches: Vec<usize>,
    settings: Settings,
    shallow: std::collections::HashSet<ObjectId>,
    /// The textconv filter of a path's diff driver, applied to every
    /// version (`--textconv`, on by default).
    pub textconv: Option<TextConv<'r>>,
    /// Line ids shared by all versions, and a scratch table that maps them
    /// to dense ids for one diff (stamp, id).
    interner: RefCell<rgit_diff::Interner>,
    dense: RefCell<(u32, Vec<(u32, u32)>)>,
}

/// Converts the contents of a path (see [`Scoreboard::textconv`]).
pub type TextConv<'r> = Box<dyn Fn(&[u8], Vec<u8>) -> Vec<u8> + 'r>;

/// Why the computation stopped.
#[derive(Debug)]
pub enum Error {
    /// An object could not be read.
    Missing(ObjectId),
}

type Res<T> = Result<T, Error>;

impl<'r> Scoreboard<'r> {
    pub fn new(repo: &'r Repo, settings: Settings) -> Scoreboard<'r> {
        let kind = repo.hash;
        let shallow = std::fs::read_to_string(repo.setup.common_dir.join("shallow"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| ObjectId::from_hex(kind, l.trim().as_bytes()).ok())
            .collect();
        Scoreboard {
            repo,
            nodes: Vec::new(),
            ids: HashMap::new(),
            origins: Vec::new(),
            origin_ids: HashMap::new(),
            pending: HashMap::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            done: Vec::new(),
            batches: Vec::new(),
            settings,
            shallow,
            textconv: None,
            interner: RefCell::new(rgit_diff::Interner::new()),
            dense: RefCell::new((0, Vec::new())),
        }
    }

    fn read(&self, id: &ObjectId) -> Res<(ObjectKind, Vec<u8>)> {
        match self.repo.odb().read(id) {
            Ok(Some(raw)) => Ok((raw.kind, raw.data)),
            _ => Err(Error::Missing(*id)),
        }
    }

    /// The node of commit `id`, loading it on first use.
    pub fn commit(&mut self, id: &ObjectId) -> Res<usize> {
        if let Some(&n) = self.ids.get(id) {
            return Ok(n);
        }
        let (kind, data) = self.read(id)?;
        if kind != ObjectKind::Commit {
            return Err(Error::Missing(*id));
        }
        let commit = Commit::parse(self.repo.hash, &data).map_err(|_| Error::Missing(*id))?;
        let time = commit
            .committer()
            .and_then(|s| s.time())
            .map_or(0, |t| t.seconds);
        let tree = commit.tree().ok();
        let parents = if self.shallow.contains(id) {
            Vec::new()
        } else {
            commit.parents().unwrap_or_default()
        };
        self.nodes.push(Node {
            id: *id,
            commit: Some(Rc::new(commit)),
            parents: Some(parents),
            tree,
            time,
            uninteresting: false,
            boundary: false,
            queued: false,
            fake_paths: None,
        });
        let n = self.nodes.len() - 1;
        self.ids.insert(*id, n);
        Ok(n)
    }

    /// The pseudo commit holding the final contents (work tree or
    /// `--contents`) with the given parents. `paths` lists the files of the
    /// pseudo commit for rename detection.
    pub fn fake_commit(
        &mut self,
        parents: Vec<ObjectId>,
        time: i64,
        paths: std::collections::BTreeMap<Vec<u8>, (u32, ObjectId)>,
    ) -> usize {
        self.nodes.push(Node {
            id: ObjectId::null(self.repo.hash),
            commit: None,
            parents: Some(parents),
            tree: None,
            time,
            uninteresting: false,
            boundary: false,
            queued: false,
            fake_paths: Some(Rc::new(paths)),
        });
        self.nodes.len() - 1
    }

    /// An origin with known contents (the final version).
    pub fn origin_with(&mut self, commit: usize, path: &[u8], blob: ObjectId, data: Rc<Blob>) -> usize {
        let o = self.origin(commit, path, blob);
        let _ = self.origins[o].content.set(data);
        o
    }

    /// Whether commit `n` has a file at `path`.
    pub fn has_path(&self, n: usize, path: &[u8]) -> Res<bool> {
        match self.nodes[n].tree {
            Some(tree) => Ok(self.lookup(tree, path)?.is_some()),
            None => Ok(false),
        }
    }

    /// The origin of the file at `path` in commit `n`, if it has one.
    pub fn final_origin(&mut self, n: usize, path: &[u8]) -> Res<Option<usize>> {
        let Some(tree) = self.nodes[n].tree else {
            return Ok(None);
        };
        Ok(self
            .lookup(tree, path)?
            .map(|(_, blob)| self.origin(n, path, blob)))
    }

    fn origin(&mut self, commit: usize, path: &[u8], blob: ObjectId) -> usize {
        let key = (commit, path.to_vec());
        if let Some(&o) = self.origin_ids.get(&key) {
            return o;
        }
        self.origins.push(Origin {
            commit,
            path: path.to_vec(),
            blob,
            content: OnceCell::new(),
            previous: None,
        });
        let o = self.origins.len() - 1;
        self.origin_ids.insert(key, o);
        o
    }

    /// The contents of origin `o`.
    pub fn content(&self, o: usize) -> Res<Rc<Blob>> {
        let origin = &self.origins[o];
        if let Some(c) = origin.content.get() {
            return Ok(c.clone());
        }
        let (_, data) = self.read(&origin.blob)?;
        let data = match &self.textconv {
            Some(conv) => conv(&origin.path, data),
            None => data,
        };
        let blob = Rc::new(Blob::new(data));
        let _ = origin.content.set(blob.clone());
        Ok(blob)
    }

    /// Mark `id` as excluded (`^<rev>`).
    pub fn exclude(&mut self, id: &ObjectId) -> Res<()> {
        let n = self.commit(id)?;
        self.nodes[n].uninteresting = true;
        self.push(n);
        Ok(())
    }

    /// Start with `entries` suspecting origin `o`.
    pub fn add(&mut self, o: usize, entries: Vec<Entry>) {
        let c = self.origins[o].commit;
        self.pending.entry(c).or_default().extend(entries);
        self.push(c);
    }

    fn push(&mut self, n: usize) {
        if self.nodes[n].queued {
            return;
        }
        self.nodes[n].queued = true;
        self.seq += 1;
        self.queue.push((self.nodes[n].time, Reverse(self.seq), n));
    }

    fn parents(&self, n: usize) -> Vec<ObjectId> {
        let mut p = self.nodes[n].parents.clone().unwrap_or_default();
        if self.settings.first_parent {
            p.truncate(1);
        }
        p
    }

    /// Run until every line is blamed.
    pub fn run(&mut self) -> Res<()> {
        while !self.pending.is_empty() {
            let Some((_, _, n)) = self.queue.pop() else {
                break;
            };
            self.nodes[n].queued = false;
            if self.nodes[n].uninteresting {
                for p in self.nodes[n].parents.clone().unwrap_or_default() {
                    let pn = self.commit(&p)?;
                    if !self.nodes[pn].uninteresting {
                        self.nodes[pn].uninteresting = true;
                        self.push(pn);
                    }
                }
            }
            let Some(entries) = self.pending.remove(&n) else {
                continue;
            };
            // Group by suspect; the suspect that appeared last goes first.
            let mut groups: Vec<(usize, Vec<Entry>)> = Vec::new();
            for e in entries {
                match groups.iter_mut().find(|g| g.0 == e.suspect) {
                    Some(g) => g.1.push(e),
                    None => groups.push((e.suspect, vec![e])),
                }
            }
            for (o, es) in groups.into_iter().rev() {
                self.pass_blame(n, o, es)?;
            }
        }
        // Anything left (cannot happen) stays with its suspect.
        for (_, es) in std::mem::take(&mut self.pending) {
            self.finish(es);
        }
        Ok(())
    }

    fn pass_blame(&mut self, n: usize, o: usize, entries: Vec<Entry>) -> Res<()> {
        let node = &self.nodes[n];
        let too_old = self.settings.since.is_some_and(|s| node.time < s);
        if node.uninteresting || too_old {
            self.nodes[n].boundary = true;
            self.finish(entries);
            return Ok(());
        }
        let parents = self.parents(n);
        if parents.is_empty() {
            if !self.settings.show_root && self.nodes[n].commit.is_some() {
                self.nodes[n].boundary = true;
            }
            self.finish(entries);
            return Ok(());
        }
        let mut porigins = Vec::with_capacity(parents.len());
        let mut pnodes = Vec::with_capacity(parents.len());
        for p in &parents {
            let pn = self.commit(p)?;
            let po = self.find_origin(pn, n, o)?;
            if let Some(po) = po
                && self.same_content(po, o)?
            {
                let moved = entries
                    .into_iter()
                    .map(|e| Entry { suspect: po, ..e })
                    .collect();
                self.add(po, moved);
                return Ok(());
            }
            porigins.push(po);
            pnodes.push(pn);
        }
        let mut rest = entries;
        for &po in porigins.iter().flatten() {
            if self.origins[o].previous.is_none() {
                self.origins[o].previous = Some(po);
            }
            rest = self.pass_to_parent(o, po, rest)?;
            if rest.is_empty() {
                break;
            }
        }
        if let Some(min) = self.settings.moves {
            for &po in porigins.iter().flatten() {
                if rest.is_empty() {
                    break;
                }
                rest = self.pass_matches(o, rest, &[po], min)?;
            }
        }
        if self.settings.copies > 0 {
            for (i, &pn) in pnodes.iter().enumerate() {
                if rest.is_empty() {
                    break;
                }
                let candidates = self.copy_candidates(pn, n, o, porigins[i])?;
                if !candidates.is_empty() {
                    let min = self.settings.copy_score;
                    rest = self.pass_matches(o, rest, &candidates, min)?;
                }
            }
        }
        self.finish(rest);
        Ok(())
    }

    /// The suspects of `entries` are final.
    fn finish(&mut self, entries: Vec<Entry>) {
        if !entries.is_empty() {
            self.batches.push(self.done.len());
            self.done.extend(entries);
        }
    }

    fn same_content(&self, a: usize, b: usize) -> Res<bool> {
        let (oa, ob) = (&self.origins[a], &self.origins[b]);
        if !oa.blob.is_null() && !ob.blob.is_null() {
            return Ok(oa.blob == ob.blob);
        }
        Ok(self.content(a)?.data == self.content(b)?.data)
    }

    /// The tree entry at `path` below `tree`.
    fn lookup(&self, tree: ObjectId, path: &[u8]) -> Res<Option<(u32, ObjectId)>> {
        let mut cur = tree;
        let comps: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
        for (i, comp) in comps.iter().enumerate() {
            let (kind, data) = self.read(&cur)?;
            if kind != ObjectKind::Tree {
                return Ok(None);
            }
            let Some(e) = TreeIter::new(self.repo.hash, &data)
                .filter_map(Result::ok)
                .find(|e| e.name == *comp)
            else {
                return Ok(None);
            };
            if i + 1 == comps.len() {
                return Ok((!e.is_tree() && e.mode_bits & 0o170000 != 0o160000)
                    .then_some((e.mode_bits, e.oid)));
            }
            if !e.is_tree() {
                return Ok(None);
            }
            cur = e.oid;
        }
        Ok(None)
    }

    /// The files of a commit.
    fn flat(&self, n: usize) -> Res<Rc<Flat>> {
        if let Some(f) = &self.nodes[n].fake_paths {
            return Ok(f.clone());
        }
        let mut out = Flat::new();
        if let Some(tree) = self.nodes[n].tree {
            self.flatten(tree, b"", &mut out)?;
        }
        Ok(Rc::new(out))
    }

    fn flatten(&self, tree: ObjectId, base: &[u8], out: &mut Flat) -> Res<()> {
        let (_, data) = self.read(&tree)?;
        for e in TreeIter::new(self.repo.hash, &data).filter_map(Result::ok) {
            let mut path = base.to_vec();
            path.extend_from_slice(e.name);
            if e.is_tree() {
                path.push(b'/');
                self.flatten(e.oid, &path, out)?;
            } else {
                out.insert(path, (e.mode_bits, e.oid));
            }
        }
        Ok(())
    }

    /// The files of `old` (path, blob) whose path is missing or different
    /// in `new` (both trees), skipping equal subtrees.
    fn changed_files(
        &self,
        old: ObjectId,
        new: Option<ObjectId>,
        base: &[u8],
        out: &mut Vec<(Vec<u8>, u32, ObjectId)>,
    ) -> Res<()> {
        let (_, data) = self.read(&old)?;
        let theirs: HashMap<Vec<u8>, (bool, ObjectId)> = match new {
            Some(t) => {
                let (_, d) = self.read(&t)?;
                TreeIter::new(self.repo.hash, &d)
                    .filter_map(Result::ok)
                    .map(|e| (e.name.to_vec(), (e.is_tree(), e.oid)))
                    .collect()
            }
            None => HashMap::new(),
        };
        for e in TreeIter::new(self.repo.hash, &data).filter_map(Result::ok) {
            let mut path = base.to_vec();
            path.extend_from_slice(e.name);
            let other = theirs.get(e.name);
            if e.is_tree() {
                path.push(b'/');
                let sub = match other {
                    Some(&(true, oid)) if oid == e.oid => continue,
                    Some(&(true, oid)) => Some(oid),
                    _ => None,
                };
                self.changed_files(e.oid, sub, &path, out)?;
            } else if other != Some(&(false, e.oid)) {
                out.push((path, e.mode_bits, e.oid));
            }
        }
        Ok(())
    }

    /// The origins in parent `pn` of commit `n` that `-C` looks for copied
    /// lines in: the files changed by the commit; with `-C -C` all files if
    /// the commit creates the file (the parent has no file at the path of
    /// `o`, even if it was renamed); with `-C -C -C` all files. The parent
    /// version `po` of the file itself is left out (`-M` handles it).
    fn copy_candidates(
        &mut self,
        pn: usize,
        n: usize,
        o: usize,
        po: Option<usize>,
    ) -> Res<Vec<usize>> {
        let all = self.settings.copies >= 3
            || (self.settings.copies == 2 && !self.has_path(pn, &self.origins[o].path.clone())?);
        let mut files: Vec<(Vec<u8>, u32, ObjectId)> = Vec::new();
        if all || self.nodes[n].tree.is_none() {
            let old = self.flat(pn)?;
            let new = if all { None } else { Some(self.flat(n)?) };
            for (p, &(mode, oid)) in old.iter() {
                if let Some(new) = &new
                    && new.get(p) == Some(&(mode, oid))
                {
                    continue;
                }
                files.push((p.clone(), mode, oid));
            }
        } else if let Some(old) = self.nodes[pn].tree {
            let new = self.nodes[n].tree;
            if new != Some(old) {
                self.changed_files(old, new, b"", &mut files)?;
            }
        }
        let skip = po.map(|po| self.origins[po].path.clone());
        let mut out = Vec::new();
        for (path, mode, oid) in files {
            if mode & 0o170000 != 0o100000 || skip.as_deref() == Some(&path[..]) {
                continue;
            }
            out.push(self.origin(pn, &path, oid));
        }
        Ok(out)
    }

    /// The version of origin `o` (in commit `n`) in parent `pn`: the same
    /// path, or the source of a rename.
    fn find_origin(&mut self, pn: usize, n: usize, o: usize) -> Res<Option<usize>> {
        let path = self.origins[o].path.clone();
        if let Some(tree) = self.nodes[pn].tree
            && let Some((_, blob)) = self.lookup(tree, &path)?
        {
            return Ok(Some(self.origin(pn, &path, blob)));
        }
        // Rename detection between the parent and the commit.
        let old = self.flat(pn)?;
        let new = self.flat(n)?;
        use rgit_diff::diffcore::{self, Detect, FilePair, Side, Status};
        let side = |p: &[u8], (mode, oid): (u32, ObjectId)| Side {
            path: p.to_vec(),
            mode,
            oid,
            extra: (),
        };
        let mut pairs: Vec<FilePair<()>> = old
            .iter()
            .filter(|(p, _)| !new.contains_key(*p))
            .map(|(p, s)| FilePair::new(Some(side(p, *s)), None))
            .collect();
        let target = match new.get(&path) {
            Some(s) => *s,
            None => (0o100644, self.origins[o].blob),
        };
        pairs.push(FilePair::new(None, Some(side(&path, target))));
        let opts = diffcore::Options {
            detect: Detect::Renames,
            ..Default::default()
        };
        let fake = self.origins[o].blob;
        let contents = |s: &Side<()>| -> Vec<u8> {
            if s.oid == fake
                && let Ok(c) = self.content(o)
            {
                return c.data.clone();
            }
            match self.repo.odb().read(&s.oid) {
                Ok(Some(raw)) if raw.kind == ObjectKind::Blob => raw.data,
                _ => Vec::new(),
            }
        };
        let (out, _) = diffcore::run(pairs, &opts, &contents);
        for p in out {
            if p.status == Status::Renamed
                && let (Some(one), Some(two)) = (&p.one, &p.two)
                && two.path == path
            {
                let (src, oid) = (one.path.clone(), one.oid);
                return Ok(Some(self.origin(pn, &src, oid)));
            }
        }
        Ok(None)
    }

    /// Pass the lines of `entries` (suspecting `o`) that are unchanged in
    /// parent origin `po` to it; returns the rest.
    fn pass_to_parent(&mut self, o: usize, po: usize, entries: Vec<Entry>) -> Res<Vec<Entry>> {
        let target = self.content(o)?;
        let parent = self.content(po)?;
        let blocks = self.common_blocks(&parent, 0..parent.len(), &target, 0..target.len());
        let mut passed = Vec::new();
        let mut rest = Vec::new();
        for e in entries {
            let end = e.s_lno + e.num;
            let mut cur = e.s_lno;
            // Blocks are sorted by target line.
            let first = blocks.partition_point(|b| b.0 + b.2 <= cur);
            for &(t, p, len) in &blocks[first..] {
                if cur >= end || t >= end {
                    break;
                }
                if t > cur {
                    rest.push(e.piece(cur, t));
                    cur = t;
                }
                let stop = end.min(t + len);
                if stop > cur {
                    let mut q = e.piece(cur, stop);
                    q.suspect = po;
                    q.s_lno = p + (cur - t);
                    passed.push(q);
                    cur = stop;
                }
            }
            if cur < end {
                rest.push(e.piece(cur, end));
            }
        }
        if !passed.is_empty() {
            self.add(po, passed);
        }
        Ok(rest)
    }

    /// `-M`/`-C`: for each entry (suspecting `o`), the best block of lines
    /// that also appears in one of `candidates`; blocks scoring at least
    /// `min` pass to their candidate and the rest of the entry is tried
    /// again, until nothing more is found. Returns what stays.
    fn pass_matches(
        &mut self,
        o: usize,
        entries: Vec<Entry>,
        candidates: &[usize],
        min: usize,
    ) -> Res<Vec<Entry>> {
        let target = self.content(o)?;
        let mut contents = Vec::with_capacity(candidates.len());
        for &c in candidates {
            contents.push(self.content(c)?);
        }
        let mut todo = entries;
        let mut stays = Vec::new();
        let mut passed: Vec<(usize, Entry)> = Vec::new();
        while !todo.is_empty() {
            let mut again = Vec::new();
            for e in todo {
                let r = e.s_lno..e.s_lno + e.num;
                let mut best: Option<Match> = None;
                for (k, cand) in contents.iter().enumerate() {
                    if let Some(m) = self.best_match(cand, &target, r.clone())
                        && best.is_none_or(|b| m.score >= b.score)
                    {
                        best = Some(Match {
                            origin: candidates[k],
                            ..m
                        });
                    }
                }
                match best {
                    Some(m) if m.score >= min => {
                        if m.t > e.s_lno {
                            again.push(e.piece(e.s_lno, m.t));
                        }
                        let mut q = e.piece(m.t, m.t + m.len);
                        q.suspect = m.origin;
                        q.s_lno = m.p;
                        passed.push((m.origin, q));
                        if m.t + m.len < r.end {
                            again.push(e.piece(m.t + m.len, r.end));
                        }
                    }
                    _ => stays.push(e),
                }
            }
            todo = again;
        }
        let mut by_origin: Vec<(usize, Vec<Entry>)> = Vec::new();
        for (c, q) in passed {
            match by_origin.iter_mut().find(|g| g.0 == c) {
                Some(g) => g.1.push(q),
                None => by_origin.push((c, vec![q])),
            }
        }
        for (c, qs) in by_origin {
            self.add(c, qs);
        }
        stays.sort_by_key(|e| e.lno);
        Ok(stays)
    }

    /// The block of lines `r` of `target` that also appears in `cand` with
    /// the highest score (`origin` is left 0).
    fn best_match(&self, cand: &Blob, target: &Blob, r: Range<usize>) -> Option<Match> {
        let blocks = self.common_blocks(cand, 0..cand.len(), target, r.clone());
        let mut best: Option<Match> = None;
        for (t, p, len) in blocks {
            let score = cand.score(p..p + len);
            if best.is_none_or(|b| score >= b.score) {
                best = Some(Match {
                    score,
                    origin: 0,
                    t: r.start + t,
                    p,
                    len,
                });
            }
        }
        best
    }

    /// The ids of the lines of `blob`.
    fn ids<'b>(&self, blob: &'b Blob) -> &'b [u32] {
        blob.ids.get_or_init(|| {
            let mut interner = self.interner.borrow_mut();
            if self.settings.ignore_whitespace {
                (0..blob.len())
                    .map(|i| interner.id(&strip_ws(blob.line(i))))
                    .collect()
            } else {
                (0..blob.len()).map(|i| interner.id(blob.line(i))).collect()
            }
        })
    }

    /// The unchanged regions between lines `pr` of `parent` and lines `tr`
    /// of `target`: (target line, parent line, length), relative to the
    /// ranges, sorted.
    fn common_blocks(
        &self,
        parent: &Blob,
        pr: Range<usize>,
        target: &Blob,
        tr: Range<usize>,
    ) -> Vec<(usize, usize, usize)> {
        let (a, b) = rgit_diff::trim_common_tail(parent.bytes(&pr), target.bytes(&tr));
        let na = parent.lines_within(&pr, a.len());
        let nb = target.lines_within(&tr, b.len());
        let ra = pr.start..pr.start + na;
        let rb = tr.start..tr.start + nb;
        let (pids, tids) = (self.ids(parent), self.ids(target));
        // Dense ids for this diff.
        let (ia, ib, classes) = {
            let mut dense = self.dense.borrow_mut();
            let (stamp, table) = &mut *dense;
            *stamp = stamp.wrapping_add(1);
            if *stamp == 0 {
                table.iter_mut().for_each(|x| *x = (0, 0));
                *stamp = 1;
            }
            let size = self.interner.borrow().classes();
            if table.len() < size {
                table.resize(size, (0, 0));
            }
            let mut next = 0u32;
            let mut map = |id: u32| -> u32 {
                let slot = &mut table[id as usize];
                if slot.0 != *stamp {
                    *slot = (*stamp, next);
                    next += 1;
                }
                slot.1
            };
            let ia: Vec<u32> = pids[ra.clone()].iter().map(|&x| map(x)).collect();
            let ib: Vec<u32> = tids[rb.clone()].iter().map(|&x| map(x)).collect();
            (ia, ib, next as usize)
        };
        // With `-w` the ids ignore whitespace; the indent heuristic looks
        // at the lines as they are.
        let la: Vec<&[u8]> = ra.clone().map(|i| parent.line(i)).collect();
        let lb: Vec<&[u8]> = rb.clone().map(|i| target.line(i)).collect();
        let changes = rgit_diff::diff_interned(&ia, &ib, classes, &la, &lb, &self.settings.diff);
        let mut out = Vec::new();
        let (mut pa, mut pb) = (0, 0);
        for c in &changes {
            if c.b_start > pb {
                out.push((pb, pa, c.b_start - pb));
            }
            pa = c.a_start + c.a_len;
            pb = c.b_start + c.b_len;
        }
        // The rest of the kept lines plus the common tail cut off before.
        if tr.len() > pb {
            out.push((pb, pa, tr.len() - pb));
        }
        out
    }
}

/// A line without whitespace (`-w`).
fn strip_ws(l: &[u8]) -> Vec<u8> {
    l.iter()
        .copied()
        .filter(|c| !c.is_ascii_whitespace() && *c != 0x0b)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_lines() {
        let b = Blob::new(b"a\nb\nc".to_vec());
        assert_eq!(b.len(), 3);
        assert_eq!(b.line(2), b"c");
        assert_eq!(Blob::new(Vec::new()).len(), 0);
        assert_eq!(b.score(0..3), 3);
        assert_eq!(b.lines_within(&(1..3), 2), 1);
        assert_eq!(b.lines_within(&(1..3), 3), 2);
    }
}
