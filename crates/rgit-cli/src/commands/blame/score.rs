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

use std::cell::OnceCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
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
}

impl Blob {
    pub fn new(data: Vec<u8>) -> Blob {
        let mut bounds = Vec::new();
        let mut start = 0;
        for (i, &b) in data.iter().enumerate() {
            if b == b'\n' {
                bounds.push((start, i + 1));
                start = i + 1;
            }
        }
        if start < data.len() {
            bounds.push((start, data.len()));
        }
        Blob { data, bounds }
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
}

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
            // Group by suspect, keeping the order of first appearance.
            let mut groups: Vec<(usize, Vec<Entry>)> = Vec::new();
            for e in entries {
                match groups.iter_mut().find(|g| g.0 == e.suspect) {
                    Some(g) => g.1.push(e),
                    None => groups.push((e.suspect, vec![e])),
                }
            }
            for (o, es) in groups {
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
        }
        let mut rest = entries;
        for po in porigins.into_iter().flatten() {
            if self.origins[o].previous.is_none() {
                self.origins[o].previous = Some(po);
            }
            rest = self.pass_to_parent(o, po, rest)?;
            if rest.is_empty() {
                break;
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
        let blocks = common_blocks(&parent, &target, &self.settings);
        let mut passed = Vec::new();
        let mut rest = Vec::new();
        for e in entries {
            let end = e.s_lno + e.num;
            let mut cur = e.s_lno;
            let piece = |from: usize, to: usize| Entry {
                lno: e.lno + (from - e.s_lno),
                num: to - from,
                suspect: e.suspect,
                s_lno: from,
            };
            // Blocks are sorted by target line.
            let first = blocks.partition_point(|b| b.0 + b.2 <= cur);
            for &(t, p, len) in &blocks[first..] {
                if cur >= end || t >= end {
                    break;
                }
                if t > cur {
                    rest.push(piece(cur, t));
                    cur = t;
                }
                let stop = end.min(t + len);
                if stop > cur {
                    let mut q = piece(cur, stop);
                    q.suspect = po;
                    q.s_lno = p + (cur - t);
                    passed.push(q);
                    cur = stop;
                }
            }
            if cur < end {
                rest.push(piece(cur, end));
            }
        }
        if !passed.is_empty() {
            self.add(po, passed);
        }
        Ok(rest)
    }
}

/// The unchanged regions between `parent` and `target`: (target line,
/// parent line, length), sorted.
fn common_blocks(parent: &Blob, target: &Blob, s: &Settings) -> Vec<(usize, usize, usize)> {
    let (a, b) = rgit_diff::trim_common_tail(&parent.data, &target.data);
    let la = rgit_diff::split_lines(a);
    let lb = rgit_diff::split_lines(b);
    let changes = if s.ignore_whitespace {
        let strip = |l: &[u8]| -> Vec<u8> {
            l.iter()
                .copied()
                .filter(|c| !c.is_ascii_whitespace() && *c != 0x0b)
                .collect()
        };
        let na: Vec<Vec<u8>> = la.iter().map(|l| strip(l)).collect();
        let nb: Vec<Vec<u8>> = lb.iter().map(|l| strip(l)).collect();
        let ra: Vec<&[u8]> = na.iter().map(Vec::as_slice).collect();
        let rb: Vec<&[u8]> = nb.iter().map(Vec::as_slice).collect();
        rgit_diff::diff_lines(&ra, &rb, &s.diff)
    } else {
        rgit_diff::diff_lines(&la, &lb, &s.diff)
    };
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
    if target.len() > pb {
        out.push((pb, pa, target.len() - pb));
    }
    out
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
    }

    #[test]
    fn blocks() {
        let p = Blob::new(b"a\nb\nc\n".to_vec());
        let t = Blob::new(b"a\nB\nc\nd\n".to_vec());
        let s = Settings::default();
        assert_eq!(common_blocks(&p, &t, &s), vec![(0, 0, 1), (2, 2, 1)]);
    }
}
