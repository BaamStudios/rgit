// SPDX-License-Identifier: MIT

//! Building `.bitmap` files.
//!
//! The layout is in `Documentation/technical/bitmap-format.adoc`. The
//! choices git makes that the format leaves open were determined from the
//! git binary (2.56):
//!
//! - **Selection.** The candidate commits are sorted by committer date,
//!   newest first (stable, so equal dates keep pack order). Then a series
//!   of windows is laid over the list: position `i` (0-based) starts a
//!   window of `offset(i) + 1` commits, where `offset(i)` is 0 up to
//!   position 100, then `min(i - 100, 100)`, and from position 20100 on
//!   `min(i - 20000, 5000)`. A window that does not fit completely is
//!   dropped. From each window the last commit is selected, unless the
//!   window holds a preferred tip (`pack.preferBitmapTips`): then the
//!   first of those.
//! - **Order and XOR.** Entries are written oldest selection first. Each
//!   bitmap is XORed with the one among the previous ten entries that
//!   gives the smallest EWAH form (the nearest on ties), if that is
//!   strictly smaller than the bitmap on its own.
//! - **Sizes.** Commit bitmaps have as many words as the object count
//!   needs (`bits` = words * 64); type bitmaps end at their last set bit.
//! - **Name hashes** are those of the path an object is first found under
//!   when the trees of the commits are walked in pack order (depth first,
//!   entries in tree order).

use std::collections::{HashMap, HashSet};

use rgit_hash::{HashKind, Hasher, ObjectId};
use rgit_object::{Commit, Tag, TreeIter};

use super::{
    Bitmap, FLAG_FULL_DAG, FLAG_HASH_CACHE, FLAG_LOOKUP_TABLE, SIGNATURE, name_hash_v1, type_slot,
};
use crate::ObjectKind;
use crate::write::ObjectSource;

/// How many earlier bitmaps are tried as XOR bases.
const XOR_WINDOW: usize = 10;

/// Options of [`serialize`].
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// `pack.writeBitmapHashCache` (default true).
    pub hash_cache: bool,
    /// `pack.writeBitmapLookupTable` (default false).
    pub lookup_table: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            hash_cache: true,
            lookup_table: false,
        }
    }
}

/// Errors of building bitmaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// An object reachable from a selected commit is not among the
    /// objects of the pack (MIDX).
    Missing(ObjectId),
    /// An object could not be read or parsed.
    Read(ObjectId, String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Missing(id) => write!(f, "object {} is missing", id.to_hex()),
            BuildError::Read(id, e) => write!(f, "unable to read {}: {e}", id.to_hex()),
        }
    }
}

impl std::error::Error for BuildError {}

/// The window that starts at sorted position `i` has `offset(i) + 1`
/// commits.
pub fn window_offset(i: usize) -> usize {
    if i <= 100 {
        0
    } else if i > 20100 {
        (i - 20000).min(5000)
    } else {
        (i - 100).min(100)
    }
}

/// Select the commits that get bitmaps. `commits` are the candidates in
/// pack order with their committer dates and whether they are merges;
/// `preferred` tells the preferred tips. Returns indices into `commits`,
/// newest first. In each window the first preferred tip wins, else the
/// last merge, else the last commit.
pub fn select_commits(
    commits: &[(ObjectId, i64, bool)],
    preferred: &dyn Fn(&ObjectId) -> bool,
) -> Vec<usize> {
    let mut sorted: Vec<usize> = (0..commits.len()).collect();
    sorted.sort_by(|&a, &b| commits[b].1.cmp(&commits[a].1));
    let mut out = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let off = window_offset(i);
        if i + off >= sorted.len() {
            break;
        }
        let window = &sorted[i..=i + off];
        let mut pick = window[off];
        for &c in window {
            if preferred(&commits[c].0) {
                pick = c;
                break;
            }
            if commits[c].2 {
                pick = c;
            }
        }
        out.push(pick);
        i += off + 1;
    }
    out
}

/// What the walk needs to know about a commit or tree.
enum Node {
    Commit { tree: ObjectId, parents: Vec<ObjectId> },
    Tree(Vec<(ObjectId, bool, Vec<u8>)>),
    Tag(ObjectId),
    Blob,
}

/// Reads and caches the commits and trees of the walk.
pub struct Graph<'a, S: ObjectSource + ?Sized> {
    source: &'a S,
    hash: HashKind,
    nodes: HashMap<ObjectId, std::sync::Arc<Node>>,
}

impl<'a, S: ObjectSource + ?Sized> Graph<'a, S> {
    /// A graph reading from `source`.
    pub fn new(source: &'a S, hash: HashKind) -> Graph<'a, S> {
        Graph {
            source,
            hash,
            nodes: HashMap::new(),
        }
    }

    fn node(&mut self, id: &ObjectId, kind: ObjectKind) -> Result<std::sync::Arc<Node>, BuildError> {
        if let Some(n) = self.nodes.get(id) {
            return Ok(n.clone());
        }
        if kind == ObjectKind::Blob {
            return Ok(std::sync::Arc::new(Node::Blob));
        }
        let obj = self
            .source
            .read(id)
            .map_err(|e| BuildError::Read(*id, e.to_string()))?
            .ok_or(BuildError::Missing(*id))?;
        let bad = || BuildError::Read(*id, "corrupt object".into());
        let node = match obj.kind {
            ObjectKind::Commit => {
                let c = Commit::parse(self.hash, &obj.data).map_err(|_| bad())?;
                Node::Commit {
                    tree: c.tree().map_err(|_| bad())?,
                    parents: c.parents().map_err(|_| bad())?,
                }
            }
            ObjectKind::Tree => {
                let mut entries = Vec::new();
                for e in TreeIter::new(self.hash, &obj.data) {
                    let e = e.map_err(|_| bad())?;
                    match e.mode_bits & 0o170000 {
                        0o160000 => {}
                        0o040000 => entries.push((e.oid, true, e.name.to_vec())),
                        _ => entries.push((e.oid, false, e.name.to_vec())),
                    }
                }
                Node::Tree(entries)
            }
            ObjectKind::Tag => {
                let t = Tag::parse(self.hash, &obj.data).map_err(|_| bad())?;
                Node::Tag(t.object().map_err(|_| bad())?)
            }
            ObjectKind::Blob => Node::Blob,
        };
        let node = std::sync::Arc::new(node);
        if !matches!(*node, Node::Blob) {
            self.nodes.insert(*id, node.clone());
        }
        Ok(node)
    }

    /// The committer date of `commit` (0 if it has none).
    pub fn commit_date(&self, commit: &ObjectId) -> Result<i64, BuildError> {
        let obj = self
            .source
            .read(commit)
            .map_err(|e| BuildError::Read(*commit, e.to_string()))?
            .ok_or(BuildError::Missing(*commit))?;
        let c = Commit::parse(self.hash, &obj.data)
            .map_err(|_| BuildError::Read(*commit, "corrupt commit".into()))?;
        Ok(c.committer().and_then(|s| s.time()).map_or(0, |t| t.seconds))
    }

    /// Parents of `commit`.
    pub fn parents(&mut self, commit: &ObjectId) -> Result<Vec<ObjectId>, BuildError> {
        match &*self.node(commit, ObjectKind::Commit)? {
            Node::Commit { parents, .. } => Ok(parents.clone()),
            _ => Err(BuildError::Read(*commit, "not a commit".into())),
        }
    }

    /// Set in `bm` the bits of everything reachable from `start` that is
    /// not set yet. Commits for which `stored` has a bitmap contribute it
    /// instead of being walked. `bit_of` maps ids to bits (a missing id is an error, or
    /// added through `extend` when given).
    pub fn fill(
        &mut self,
        start: &[(ObjectId, ObjectKind)],
        bm: &mut Bitmap,
        stored: &mut dyn FnMut(&ObjectId) -> Option<std::sync::Arc<Bitmap>>,
        bit_of: &mut dyn FnMut(&ObjectId, ObjectKind) -> Option<usize>,
    ) -> Result<(), BuildError> {
        let mut stack: Vec<(ObjectId, ObjectKind)> = start.to_vec();
        while let Some((id, kind)) = stack.pop() {
            let bit = bit_of(&id, kind).ok_or(BuildError::Missing(id))?;
            if bm.get(bit) {
                continue;
            }
            if kind == ObjectKind::Commit
                && let Some(b) = stored(&id)
            {
                bm.or(&b);
                continue;
            }
            bm.set(bit);
            match &*self.node(&id, kind)? {
                Node::Commit { tree, parents } => {
                    stack.push((*tree, ObjectKind::Tree));
                    for p in parents.iter().rev() {
                        stack.push((*p, ObjectKind::Commit));
                    }
                }
                Node::Tree(entries) => {
                    for (oid, is_tree, _) in entries.iter().rev() {
                        let k = if *is_tree {
                            ObjectKind::Tree
                        } else {
                            ObjectKind::Blob
                        };
                        stack.push((*oid, k));
                    }
                }
                Node::Tag(target) => {
                    let k = self
                        .source
                        .kind(target)
                        .map_err(|e| BuildError::Read(*target, e.to_string()))?
                        .ok_or(BuildError::Missing(*target))?;
                    stack.push((*target, k));
                }
                Node::Blob => {}
            }
        }
        Ok(())
    }

    /// Name hashes (by bit) of the objects reached from the trees of
    /// `commits` (in the given order): the hash of the path each object is
    /// first found under. Objects not found keep 0.
    pub fn name_hashes(
        &mut self,
        commits: &[ObjectId],
        n: usize,
        bit_of: &dyn Fn(&ObjectId) -> Option<usize>,
    ) -> Result<Vec<u32>, BuildError> {
        let mut out = vec![0u32; n];
        let mut seen: HashSet<ObjectId> = HashSet::new();
        for c in commits {
            let tree = match &*self.node(c, ObjectKind::Commit)? {
                Node::Commit { tree, .. } => *tree,
                _ => continue,
            };
            if !seen.insert(tree) {
                continue;
            }
            // Depth first: (tree, next entry, path of the tree).
            let root = self.node(&tree, ObjectKind::Tree)?;
            let mut stack: Vec<(std::sync::Arc<Node>, usize, Vec<u8>)> = vec![(root, 0, Vec::new())];
            while let Some((node, pos, path)) = stack.last_mut() {
                let Node::Tree(entries) = &**node else {
                    stack.pop();
                    continue;
                };
                let Some((oid, is_tree, name)) = entries.get(*pos) else {
                    stack.pop();
                    continue;
                };
                *pos += 1;
                let (oid, is_tree) = (*oid, *is_tree);
                if !seen.insert(oid) {
                    continue;
                }
                let mut full = path.clone();
                if !full.is_empty() {
                    full.push(b'/');
                }
                full.extend_from_slice(name);
                if let Some(b) = bit_of(&oid) {
                    out[b] = name_hash_v1(&full);
                }
                if is_tree {
                    let child = self.node(&oid, ObjectKind::Tree)?;
                    stack.push((child, 0, full));
                }
            }
        }
        Ok(out)
    }
}

/// One selected commit and its bitmap.
pub struct Selected {
    /// The commit.
    pub id: ObjectId,
    /// Objects reachable from it.
    pub bitmap: std::sync::Arc<Bitmap>,
}

/// Compute the bitmaps of `selected`, in that order (the order they are
/// written in). `bit_of` maps the ids of the pack (MIDX) to bits. A
/// bitmap has as many words as its last set bit needs.
pub fn build<S: ObjectSource + ?Sized>(
    graph: &mut Graph<'_, S>,
    selected: &[ObjectId],
    num_objects: usize,
    bit_of: &dyn Fn(&ObjectId) -> Option<usize>,
) -> Result<Vec<Selected>, BuildError> {
    let mut done: HashMap<ObjectId, std::sync::Arc<Bitmap>> = HashMap::new();
    let mut out = Vec::with_capacity(selected.len());
    let _ = num_objects;
    for c in selected {
        let mut bm = Bitmap::new();
        let mut lookup = |id: &ObjectId, _k: ObjectKind| bit_of(id);
        let mut stored = |id: &ObjectId| done.get(id).cloned();
        graph.fill(&[(*c, ObjectKind::Commit)], &mut bm, &mut stored, &mut lookup)?;
        bm.trim();
        let bm = std::sync::Arc::new(bm);
        done.insert(*c, bm.clone());
        out.push(Selected { id: *c, bitmap: bm });
    }
    Ok(out)
}

/// The bytes of a `.bitmap` file.
///
/// - `kinds`: the type of every object, by bit;
/// - `index_pos`: the position of every object in the `.idx` (MIDX), by
///   bit;
/// - `entries`: the commit bitmaps, oldest first (the order of [`build`]);
/// - `name_hashes`: by bit (written in index order when the cache is on);
/// - `checksum`: of the pack (MIDX).
pub fn serialize(
    hash: HashKind,
    kinds: &[ObjectKind],
    index_pos: &[u32],
    entries: &[Selected],
    entry_pos: &dyn Fn(&ObjectId) -> u32,
    name_hashes: &[u32],
    checksum: &[u8],
    opts: Options,
) -> Vec<u8> {
    let n = kinds.len();
    let mut flags = FLAG_FULL_DAG;
    if opts.hash_cache {
        flags |= FLAG_HASH_CACHE;
    }
    if opts.lookup_table {
        flags |= FLAG_LOOKUP_TABLE;
    }
    let mut out = Vec::new();
    out.extend_from_slice(SIGNATURE);
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    out.extend_from_slice(checksum);

    let mut types: [Bitmap; 4] = Default::default();
    for (bit, k) in kinds.iter().enumerate() {
        types[type_slot(*k)].set(bit);
    }
    for t in &types {
        t.encode_trimmed(&mut out);
    }

    // (entry start offset, xor row) per entry.
    let mut written: Vec<(usize, Option<usize>)> = Vec::with_capacity(entries.len());
    for (k, e) in entries.iter().enumerate() {
        let raw = e.bitmap.compressed_words();
        let mut best: Option<(usize, usize, Bitmap)> = None;
        for j in 1..=k.min(XOR_WINDOW) {
            let mut x = (*e.bitmap).clone();
            x.xor(&entries[k - j].bitmap);
            let size = x.compressed_words();
            if best.as_ref().is_none_or(|b| size < b.0) {
                best = Some((size, j, x));
            }
        }
        let start = out.len();
        out.extend_from_slice(&entry_pos(&e.id).to_be_bytes());
        match best {
            Some((size, j, x)) if size < raw => {
                out.push(j as u8);
                out.push(0);
                x.encode_full(&mut out);
                written.push((start, Some(k - j)));
            }
            _ => {
                out.push(0);
                out.push(0);
                e.bitmap.encode_full(&mut out);
                written.push((start, None));
            }
        }
    }

    if opts.lookup_table {
        let mut rows: Vec<usize> = (0..entries.len()).collect();
        rows.sort_by_key(|&i| entry_pos(&entries[i].id));
        let mut row_of = vec![0u32; entries.len()];
        for (r, &i) in rows.iter().enumerate() {
            row_of[i] = r as u32;
        }
        for &i in &rows {
            out.extend_from_slice(&entry_pos(&entries[i].id).to_be_bytes());
            out.extend_from_slice(&(written[i].0 as u64).to_be_bytes());
            let x = written[i].1.map_or(u32::MAX, |b| row_of[b]);
            out.extend_from_slice(&x.to_be_bytes());
        }
    }

    if opts.hash_cache {
        let mut cache = vec![0u32; n];
        for bit in 0..n {
            cache[index_pos[bit] as usize] = name_hashes.get(bit).copied().unwrap_or(0);
        }
        for h in cache {
            out.extend_from_slice(&h.to_be_bytes());
        }
    }

    let mut hasher = Hasher::new(hash);
    hasher.update(&out);
    if let Ok(sum) = hasher.finalize() {
        out.extend_from_slice(sum.as_bytes());
    }
    out
}

/// The objects a bitmap is written for.
pub struct Objects<'a> {
    /// Id and type of every object, by bit.
    pub objects: &'a [(ObjectId, ObjectKind)],
    /// Position in the `.idx` (MIDX) of every object, by bit.
    pub index_pos: &'a [u32],
    /// Checksum of the pack (MIDX).
    pub checksum: &'a [u8],
}

/// Where the name hashes come from.
pub enum NameHashes {
    /// Walk the trees of the candidate commits.
    Walk,
    /// Given, by bit.
    Given(Vec<u32>),
}

/// What [`write_bitmap`] did.
#[derive(Debug, Default, Clone)]
pub struct Written {
    /// The file content.
    pub bytes: Vec<u8>,
    /// Number of selected commits.
    pub selected: usize,
    /// Number of maximal commits: the selected ones plus those below
    /// which the nearest selected (or maximal) descendants differ.
    pub maximal: usize,
}

/// Count the maximal commits of `selected`: walking from the selected
/// commits towards the roots (children first), a commit is maximal when
/// it is selected or when its children lead to different maximal
/// commits.
pub fn count_maximal<S: ObjectSource + ?Sized>(
    graph: &mut Graph<'_, S>,
    selected: &[ObjectId],
) -> Result<usize, BuildError> {
    // The commits reachable from the selected ones, with their parents.
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut stack: Vec<ObjectId> = selected.to_vec();
    while let Some(c) = stack.pop() {
        if parents.contains_key(&c) {
            continue;
        }
        let ps = graph.parents(&c)?;
        stack.extend(ps.iter().copied());
        parents.insert(c, ps);
    }
    let mut children_left: HashMap<ObjectId, usize> = parents.keys().map(|c| (*c, 0)).collect();
    for ps in parents.values() {
        for p in ps {
            *children_left.get_mut(p).expect("walked") += 1;
        }
    }
    let is_selected: HashSet<ObjectId> = selected.iter().copied().collect();
    let mut incoming: HashMap<ObjectId, HashSet<ObjectId>> = HashMap::new();
    let mut ready: Vec<ObjectId> = children_left
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(c, _)| *c)
        .collect();
    ready.sort();
    let mut count = 0;
    while let Some(c) = ready.pop() {
        let got = incoming.remove(&c).unwrap_or_default();
        let ident = if is_selected.contains(&c) || got.len() > 1 {
            count += 1;
            Some(c)
        } else {
            got.into_iter().next()
        };
        for p in &parents[&c] {
            if let Some(i) = ident {
                incoming.entry(*p).or_default().insert(i);
            }
            let n = children_left.get_mut(p).expect("walked");
            *n -= 1;
            if *n == 0 {
                ready.push(*p);
            }
        }
    }
    Ok(count)
}

/// Build a complete `.bitmap` file. `candidates` are the commits that may
/// be selected, in pack (walk) order; `preferred` tells the preferred
/// tips.
pub fn write_bitmap<S: ObjectSource + ?Sized>(
    source: &S,
    hash: HashKind,
    objs: &Objects<'_>,
    candidates: &[ObjectId],
    preferred: &dyn Fn(&ObjectId) -> bool,
    names: NameHashes,
    opts: Options,
) -> Result<Written, BuildError> {
    let n = objs.objects.len();
    let bit_map: HashMap<ObjectId, usize> = objs
        .objects
        .iter()
        .enumerate()
        .map(|(b, (id, _))| (*id, b))
        .collect();
    let bit_of = |id: &ObjectId| bit_map.get(id).copied();
    let mut graph = Graph::new(source, hash);
    let mut dated = Vec::with_capacity(candidates.len());
    for c in candidates {
        let merge = graph.parents(c)?.len() > 1;
        dated.push((*c, graph.commit_date(c)?, merge));
    }
    let mut picked = select_commits(&dated, preferred);
    // Written oldest first; equal dates keep pack order.
    picked.sort_by_key(|&i| (dated[i].1, i));
    let selected: Vec<ObjectId> = picked.iter().map(|&i| dated[i].0).collect();
    let maximal = count_maximal(&mut graph, &selected)?;
    let entries = build(&mut graph, &selected, n, &bit_of)?;
    let hashes = match names {
        NameHashes::Given(v) => v,
        NameHashes::Walk if opts.hash_cache => {
            let mut v = graph.name_hashes(candidates, n, &bit_of)?;
            // Annotated tags are named by their `tag` header.
            for (bit, (id, kind)) in objs.objects.iter().enumerate() {
                if *kind != ObjectKind::Tag {
                    continue;
                }
                if let Ok(Some(obj)) = source.read(id)
                    && let Ok(t) = Tag::parse(hash, &obj.data)
                {
                    v[bit] = t.name().map_or(0, name_hash_v1);
                }
            }
            v
        }
        NameHashes::Walk => Vec::new(),
    };
    let kinds: Vec<ObjectKind> = objs.objects.iter().map(|o| o.1).collect();
    let entry_pos = |id: &ObjectId| bit_of(id).map_or(0, |b| objs.index_pos[b]);
    let bytes = serialize(
        hash,
        &kinds,
        objs.index_pos,
        &entries,
        &entry_pos,
        &hashes,
        objs.checksum,
        opts,
    );
    Ok(Written {
        bytes,
        selected: selected.len(),
        maximal,
    })
}

/// [`write_bitmap`] for one pack: every commit of the pack is a
/// candidate. `walk_order` is the order the commits were found in when
/// the pack was built (git's revision walk order); without it, pack order
/// is used.
pub fn write_pack_bitmap<S: ObjectSource + ?Sized>(
    pack: &crate::Pack,
    source: &S,
    walk_order: Option<&[ObjectId]>,
    preferred: &dyn Fn(&ObjectId) -> bool,
    opts: Options,
) -> Result<Written, BuildError> {
    let idx = pack.index();
    let hash = idx.hash_kind();
    let mut by_offset: Vec<(u64, u32)> = Vec::with_capacity(idx.len() as usize);
    for p in 0..idx.len() {
        let off = idx
            .offset_at(p)
            .map_err(|e| BuildError::Read(idx.id_at(p), e.to_string()))?;
        by_offset.push((off, p));
    }
    by_offset.sort_unstable();
    let mut objects = Vec::with_capacity(by_offset.len());
    let mut index_pos = Vec::with_capacity(by_offset.len());
    let mut commits: Vec<ObjectId> = Vec::new();
    for &(off, p) in &by_offset {
        let id = idx.id_at(p);
        let (kind, _) = pack
            .object_info(off)
            .map_err(|e| BuildError::Read(id, e.to_string()))?;
        if kind == ObjectKind::Commit {
            commits.push(id);
        }
        objects.push((id, kind));
        index_pos.push(p);
    }
    if let Some(order) = walk_order {
        let in_pack: HashSet<ObjectId> = commits.iter().copied().collect();
        let mut listed: HashSet<ObjectId> = HashSet::new();
        let mut ordered: Vec<ObjectId> = order
            .iter()
            .filter(|c| in_pack.contains(c) && listed.insert(**c))
            .copied()
            .collect();
        ordered.extend(commits.iter().filter(|c| !listed.contains(c)));
        commits = ordered;
    }
    let objs = Objects {
        objects: &objects,
        index_pos: &index_pos,
        checksum: idx.pack_checksum(),
    };
    write_bitmap(source, hash, &objs, &commits, preferred, NameHashes::Walk, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        let ids: Vec<(ObjectId, i64, bool)> = (0..1000)
            .map(|i| (ObjectId::null(HashKind::Sha1), 1000 - i as i64, false))
            .collect();
        let sel = select_commits(&ids, &|_| false);
        assert_eq!(&sel[..3], &[0, 1, 2]);
        let tail: Vec<usize> = sel[100..].to_vec();
        assert_eq!(
            tail,
            [100, 102, 106, 114, 130, 162, 226, 327, 428, 529, 630, 731, 832, 933]
        );
    }

    #[test]
    fn far_windows() {
        let ids: Vec<(ObjectId, i64, bool)> = (0..40000)
            .map(|i| (ObjectId::null(HashKind::Sha1), -(i as i64), false))
            .collect();
        let sel = select_commits(&ids, &|_| false);
        let pos = sel.iter().position(|&s| s == 20123).unwrap();
        assert_eq!(
            &sel[pos..],
            &[20123, 20248, 20498, 20998, 21998, 23998, 27998, 32999, 38000]
        );
    }

    #[test]
    fn name_hash() {
        assert_eq!(name_hash_v1(b""), 0);
        assert_eq!(name_hash_v1(b"a"), u32::from(b'a') << 24);
        assert_eq!(name_hash_v1(b"a b"), name_hash_v1(b"ab"));
    }
}
