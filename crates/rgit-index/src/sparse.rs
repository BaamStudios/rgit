// SPDX-License-Identifier: MIT

//! Sparse directory entries (`sdir` extension, gitformat-index(5) "Sparse
//! Directory Entries", Documentation/technical/sparse-index.adoc).
//!
//! A sparse index stores a directory outside the sparse-checkout cone as one
//! entry: mode `040000`, the path with a trailing `/`, the tree's object id
//! and the skip-worktree bit. rgit works on the whole list of files in
//! memory: [`Index::expand_sparse`] replaces every sparse directory entry
//! with the files of its tree (as skip-worktree entries without stat data),
//! and [`Index::collapse_sparse`] turns directories back into sparse
//! directory entries before the index is written.
//!
//! Reading through [`Index::read_file`] expands automatically when the
//! program registered a tree reader with [`set_tree_reader`] (and
//! [`ReadOptions::keep_sparse`](crate::ReadOptions) is off); the index then
//! remembers what the file held ([`Index::sparse_origin`]).

use std::cell::RefCell;

use rgit_hash::{HashKind, ObjectId};

use crate::entry::{Entry, Flags, Mode, Stage, Stat};
use crate::tree::TreeNode;
use crate::{Error, Index};

/// Reads the raw data of a tree object for the index file at the given
/// path.
pub type TreeReader = Box<dyn Fn(&std::path::Path, &ObjectId) -> Option<Vec<u8>>>;

thread_local! {
    static TREE_READER: RefCell<Option<TreeReader>> = const { RefCell::new(None) };
}

/// Registers the function that reads tree objects for expanding sparse
/// directory entries when an index file is read (see the module
/// documentation). Replaces an earlier one; `None` removes it.
pub fn set_tree_reader(reader: Option<TreeReader>) {
    TREE_READER.with(|r| *r.borrow_mut() = reader);
}

/// Turns the index about to be written to the given file into the form
/// to write (e.g. collapsed into a sparse index), or `None` to write it as
/// it is.
pub type WriteTransform = Box<dyn Fn(&std::path::Path, &Index) -> Option<Index>>;

thread_local! {
    static WRITE_TRANSFORM: RefCell<Option<WriteTransform>> = const { RefCell::new(None) };
}

/// Registers the function [`Index::write_file`] and [`Index::write_locked`]
/// pass every index through before writing it (see [`WriteTransform`]).
pub fn set_write_transform(transform: Option<WriteTransform>) {
    WRITE_TRANSFORM.with(|t| *t.borrow_mut() = transform);
}

/// The index to write to `path` instead of `index`, if the registered
/// transform changes it.
pub(crate) fn transform_for_write(path: &std::path::Path, index: &Index) -> Option<Index> {
    WRITE_TRANSFORM.with(|t| t.borrow().as_ref().and_then(|f| f(path, index)))
}

/// Expands `index`, read from `path`, with the registered tree reader, if
/// there is one.
pub(crate) fn expand_with_reader(path: &std::path::Path, index: &mut Index) -> Result<(), Error> {
    TREE_READER.with(|r| match r.borrow().as_ref() {
        Some(read) => index.expand_sparse(&mut |id| read(path, id)),
        None => Ok(()),
    })
}

/// True while a tree reader is registered.
pub fn has_tree_reader() -> bool {
    TREE_READER.with(|r| r.borrow().is_some())
}

/// What a sparse index file held before [`Index::expand_sparse`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SparseOrigin {
    /// The sparse directory entries (paths with the trailing `/`), sorted.
    pub dirs: Vec<Vec<u8>>,
    /// The paths of all other entries, sorted (with duplicates for the
    /// stages of a conflict).
    pub files: Vec<Vec<u8>>,
}

impl SparseOrigin {
    /// The origin of an index as it is (sparse directory entries and
    /// other entries).
    pub fn of(index: &Index) -> SparseOrigin {
        let mut origin = SparseOrigin::default();
        for e in &index.entries {
            if e.mode.is_dir() {
                origin.dirs.push(e.path.clone());
            } else {
                origin.files.push(e.path.clone());
            }
        }
        origin
    }

    /// True if an entry other than a sparse directory lies below `dir`
    /// (no trailing slash).
    pub fn has_file_below(&self, dir: &[u8]) -> bool {
        let mut prefix = dir.to_vec();
        prefix.push(b'/');
        let i = self
            .files
            .partition_point(|p| p.as_slice() < prefix.as_slice());
        self.files.get(i).is_some_and(|p| p.starts_with(&prefix))
    }

    /// The sparse directory (with trailing slash) that contains `path`,
    /// if any.
    pub fn dir_containing(&self, path: &[u8]) -> Option<&[u8]> {
        let i = self.dirs.partition_point(|d| d.as_slice() <= path);
        // Directories containing `path` sort before it; the closest one is
        // the last directory that is a prefix.
        self.dirs[..i]
            .iter()
            .rev()
            .take_while(|d| path.first() == d.first())
            .find(|d| path.starts_with(d))
            .map(Vec::as_slice)
    }
}

/// What [`Index::collapse_sparse`] may turn into sparse directory entries.
pub struct CollapseRules<'a> {
    /// The directory (no trailing slash) lies entirely outside the
    /// sparse-checkout cone.
    pub outside: &'a dyn Fn(&[u8]) -> bool,
    /// The directory must stay expanded even though it could be collapsed
    /// (git keeps directories that a sparse index already listed file by
    /// file).
    pub keep: &'a dyn Fn(&[u8]) -> bool,
}

impl Index {
    /// True if the index has sparse directory entries.
    pub fn is_sparse(&self) -> bool {
        self.entries.iter().any(|e| e.mode.is_dir())
    }

    /// True for a sparse index: one read with the `sdir` extension (and not
    /// expanded since) or made by [`Index::collapse_sparse`], even without
    /// sparse directory entries. Such an index is written with `sdir`.
    pub fn is_sparse_index(&self) -> bool {
        self.sparse || self.is_sparse()
    }

    /// The sparse directory entries the index file held, if it was a
    /// sparse index that has been expanded on reading.
    pub fn sparse_origin(&self) -> Option<&SparseOrigin> {
        self.sparse_origin.as_deref()
    }

    /// Sets (or clears) the recorded origin.
    pub fn set_sparse_origin(&mut self, origin: Option<SparseOrigin>) {
        self.sparse_origin = origin.map(Box::new);
    }

    /// Replaces every sparse directory entry by the files of its tree,
    /// read with `read` (raw tree data). The new entries are skip-worktree
    /// entries without stat data; the cache tree learns the subtrees.
    /// Records the previous layout as [`Index::sparse_origin`].
    pub fn expand_sparse(
        &mut self,
        read: &mut dyn FnMut(&ObjectId) -> Option<Vec<u8>>,
    ) -> Result<(), Error> {
        if !self.is_sparse() {
            if self.sparse {
                self.sparse_origin = Some(Box::new(SparseOrigin::of(self)));
                self.sparse = false;
            }
            return Ok(());
        }
        self.sparse = false;
        let origin = SparseOrigin::of(self);
        let kind = self.kind;
        let old = std::mem::take(&mut self.entries);
        let mut entries = Vec::with_capacity(old.len());
        for e in old {
            if !e.mode.is_dir() {
                entries.push(e);
                continue;
            }
            let dir = e.path.strip_suffix(b"/").unwrap_or(&e.path).to_vec();
            let mut files = Vec::new();
            let node = list_tree(kind, &e.id, &dir, read, &mut files, 0)?;
            for (path, mode, id) in files {
                let mut entry = Entry::new(path, Stage::MERGED, mode, id);
                entry.flags = Flags {
                    skip_worktree: true,
                    ..e.flags
                };
                entries.push(entry);
            }
            if let Some(tree) = &mut self.tree {
                expand_node(&mut tree.root, &dir, node);
            }
        }
        // The files of "a/" sort exactly where "a/" did: no other entry
        // starts with "a/", so the order is kept.
        self.entries = entries;
        self.sparse_origin = Some(Box::new(origin));
        Ok(())
    }

    /// A copy of the index in which directories are replaced by sparse
    /// directory entries where allowed: the directory lies outside the
    /// cone (`rules.outside`), is not to be kept (`rules.keep`), all its
    /// entries are merged skip-worktree files (no intent-to-add entries or
    /// submodules), and the cache tree has a valid node covering exactly
    /// these entries. Directories whose cache tree node is invalid are
    /// left alone with everything below them; without a valid root nothing
    /// changes.
    pub fn collapse_sparse(&self, rules: &CollapseRules<'_>) -> Index {
        let mut out = self.clone();
        let Some(tree) = &self.tree else {
            return out;
        };
        if !tree.root.is_valid() {
            return out;
        }
        out.sparse = true;
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut collapsed = Vec::new();
        walk(
            &self.entries,
            &tree.root,
            &[],
            rules,
            &mut entries,
            &mut collapsed,
        );
        if collapsed.is_empty() {
            return out;
        }
        out.entries = entries;
        if let Some(tree) = &mut out.tree {
            for (dir, count) in &collapsed {
                collapse_node(&mut tree.root, dir, *count);
            }
        }
        out
    }
}

fn walk(
    entries: &[Entry],
    node: &TreeNode,
    prefix: &[u8],
    rules: &CollapseRules<'_>,
    out: &mut Vec<Entry>,
    collapsed: &mut Vec<(Vec<u8>, u32)>,
) {
    let mut i = 0;
    while i < entries.len() {
        let rest = &entries[i].path[prefix.len()..];
        let slash = match rest.iter().position(|&b| b == b'/') {
            Some(s) if s + 1 < rest.len() => s,
            _ => {
                out.push(entries[i].clone());
                i += 1;
                continue;
            }
        };
        let name = &rest[..slash];
        let mut dir_prefix = entries[i].path[..prefix.len() + slash + 1].to_vec();
        let mut j = i + 1;
        while j < entries.len() && entries[j].path.starts_with(&dir_prefix) {
            j += 1;
        }
        let group = &entries[i..j];
        let dir = &dir_prefix[..dir_prefix.len() - 1];
        match node.child(name).filter(|c| c.is_valid()) {
            None => out.extend_from_slice(group),
            Some(child) => {
                let collapsible = (rules.outside)(dir)
                    && !(rules.keep)(dir)
                    && child.entry_count() == u32::try_from(group.len()).ok()
                    && group.iter().all(|e| {
                        e.stage == Stage::MERGED
                            && e.flags.skip_worktree
                            && !e.flags.intent_to_add
                            && !e.mode.is_gitlink()
                            && !e.mode.is_dir()
                    });
                if collapsible {
                    let id = *child.id().expect("valid node");
                    let mut entry = Entry::new(dir_prefix.clone(), Stage::MERGED, Mode::DIR, id);
                    entry.flags.skip_worktree = true;
                    entry.stat = Stat::default();
                    out.push(entry);
                    collapsed.push((dir.to_vec(), child.entry_count().unwrap_or(1)));
                } else {
                    let p = std::mem::take(&mut dir_prefix);
                    walk(group, child, &p, rules, out, collapsed);
                }
            }
        }
        i = j;
    }
}

/// Lists the files of tree `id` below `dir` into `out` and returns the
/// cache tree node describing it.
fn list_tree(
    kind: HashKind,
    id: &ObjectId,
    dir: &[u8],
    read: &mut dyn FnMut(&ObjectId) -> Option<Vec<u8>>,
    out: &mut Vec<(Vec<u8>, Mode, ObjectId)>,
    depth: usize,
) -> Result<TreeNode, Error> {
    if depth > 4096 {
        return Err(Error::Corrupt("sparse directory nesting"));
    }
    let data = read(id).ok_or(Error::Corrupt("sparse directory tree"))?;
    let start = out.len();
    let name = dir.rsplit(|&b| b == b'/').next().unwrap_or(dir);
    let mut children = Vec::new();
    let hash_len = kind.byte_len();
    let mut pos = 0;
    while pos < data.len() {
        let sp = data[pos..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or(Error::Corrupt("sparse directory tree"))?;
        let mode = std::str::from_utf8(&data[pos..pos + sp])
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or(Error::Corrupt("sparse directory tree"))?;
        pos += sp + 1;
        let nul = data[pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::Corrupt("sparse directory tree"))?;
        let entry_name = &data[pos..pos + nul];
        pos += nul + 1;
        let oid = data
            .get(pos..pos + hash_len)
            .and_then(|b| ObjectId::from_bytes(kind, b).ok())
            .ok_or(Error::Corrupt("sparse directory tree"))?;
        pos += hash_len;
        let mut path = dir.to_vec();
        path.push(b'/');
        path.extend_from_slice(entry_name);
        if mode & 0o170000 == 0o040000 {
            children.push(list_tree(kind, &oid, &path, read, out, depth + 1)?);
        } else {
            out.push((path, Mode(canonical_mode(mode)), oid));
        }
    }
    // Tree order puts "a.b" before the directory "a"; the index sorts by
    // full path.
    out[start..].sort_by(|a, b| a.0.cmp(&b.0));
    children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    let count = u32::try_from(out.len() - start).unwrap_or(u32::MAX);
    let mut node = TreeNode::new_valid(name.to_vec(), count, *id);
    node.children = children;
    Ok(node)
}

/// The mode an index entry gets for a tree entry's mode.
fn canonical_mode(mode: u32) -> u32 {
    match mode & 0o170000 {
        0o120000 => 0o120000,
        0o160000 => 0o160000,
        _ if mode & 0o100 != 0 => 0o100755,
        _ => 0o100644,
    }
}

/// Puts the node of expanded directory `dir` into the cache tree and adds
/// the new entries to the counts of the directories above it; invalidates
/// them when the cache tree does not know the directory.
fn expand_node(root: &mut TreeNode, dir: &[u8], node: TreeNode) {
    let added = node.entry_count().unwrap_or(1).saturating_sub(1);
    let comps: Vec<&[u8]> = dir.split(|&b| b == b'/').collect();
    // Check the whole chain first.
    let mut ok = true;
    {
        let mut n: &TreeNode = root;
        for c in &comps {
            match n.child(c) {
                Some(child) => n = child,
                None => {
                    ok = false;
                    break;
                }
            }
        }
    }
    if !ok {
        let mut n = root;
        n.invalidate();
        for c in &comps {
            match n.child_mut(c) {
                Some(child) => {
                    child.invalidate();
                    n = child;
                }
                None => return,
            }
        }
        return;
    }
    let mut n = root;
    for c in &comps {
        if let Some(count) = n.entry_count() {
            let id = *n.id().expect("valid");
            n.set_valid(count + added, id);
        }
        n = n.child_mut(c).expect("checked");
    }
    *n = node;
}

/// Turns the node of collapsed directory `dir` (which covered `count`
/// entries) into a leaf covering one entry and updates the counts above.
fn collapse_node(root: &mut TreeNode, dir: &[u8], count: u32) {
    let removed = count.saturating_sub(1);
    let mut n = root;
    for c in dir.split(|&b| b == b'/') {
        if let Some(cnt) = n.entry_count() {
            let id = *n.id().expect("valid");
            n.set_valid(cnt.saturating_sub(removed), id);
        }
        match n.child_mut(c) {
            Some(child) => n = child,
            None => return,
        }
    }
    if let Some(id) = n.id().copied() {
        n.set_valid(1, id);
    }
    n.children.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CacheTree, ReadOptions, WriteOptions};

    fn oid(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    fn tree_data(items: &[(&str, &[u8], ObjectId)]) -> Vec<u8> {
        let mut v = Vec::new();
        for (mode, name, id) in items {
            v.extend_from_slice(mode.as_bytes());
            v.push(b' ');
            v.extend_from_slice(name);
            v.push(0);
            v.extend_from_slice(id.as_bytes());
        }
        v
    }

    fn sparse_sample() -> Index {
        let mut index = Index::new(HashKind::Sha1);
        index.sparse = true;
        let mut d = Entry::new(b"a/".to_vec(), Stage::MERGED, Mode::DIR, oid(10));
        d.flags.skip_worktree = true;
        index.entries.push(d);
        index.entries.push(Entry::new(
            b"c/h".to_vec(),
            Stage::MERGED,
            Mode::FILE,
            oid(3),
        ));
        let mut root = TreeNode::new_valid("", 2, oid(1));
        root.children.push(TreeNode::new_valid("a", 1, oid(10)));
        root.children.push(TreeNode::new_valid("c", 1, oid(2)));
        index.tree = Some(CacheTree { root });
        index
    }

    fn reader(id: &ObjectId) -> Option<Vec<u8>> {
        if *id == oid(10) {
            Some(tree_data(&[
                ("100644", b"g", oid(4)),
                ("40000", b"x", oid(11)),
            ]))
        } else if *id == oid(11) {
            Some(tree_data(&[("100755", b"f", oid(5))]))
        } else {
            None
        }
    }

    #[test]
    fn round_trip_on_disk() {
        let index = sparse_sample();
        let bytes = index.to_bytes(&WriteOptions::default());
        assert_eq!(&bytes[4..8], &3u32.to_be_bytes());
        let read = Index::from_bytes(&bytes, HashKind::Sha1, &ReadOptions::default()).unwrap();
        assert!(read.is_sparse());
        assert_eq!(read.to_bytes(&WriteOptions::default()), bytes);
    }

    #[test]
    fn expand_and_collapse() {
        let mut index = sparse_sample();
        index.expand_sparse(&mut |id| reader(id)).unwrap();
        let paths: Vec<_> = index.entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths, [b"a/g".to_vec(), b"a/x/f".to_vec(), b"c/h".to_vec()]);
        assert!(index.entries[0].flags.skip_worktree);
        assert_eq!(index.entries[1].mode, Mode::EXECUTABLE);
        let tree = index.cache_tree().unwrap();
        assert_eq!(tree.root.entry_count(), Some(3));
        assert_eq!(tree.find(b"a/x").unwrap().entry_count(), Some(1));
        let origin = index.sparse_origin().unwrap();
        assert_eq!(origin.dirs, [b"a/".to_vec()]);
        assert_eq!(origin.dir_containing(b"a/x/f"), Some(&b"a/"[..]));
        assert_eq!(origin.dir_containing(b"c/h"), None);

        let outside = |d: &[u8]| d.starts_with(b"a");
        let keep = |_: &[u8]| false;
        let rules = CollapseRules {
            outside: &outside,
            keep: &keep,
        };
        let mut back = index.collapse_sparse(&rules);
        back.sparse_origin = None;
        assert_eq!(back, sparse_sample());

        // Not collapsed when an entry is not skip-worktree; the subtree is.
        index.entries[0].flags.skip_worktree = false;
        let partial = index.collapse_sparse(&rules);
        let paths: Vec<_> = partial.entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths, [b"a/g".to_vec(), b"a/x/".to_vec(), b"c/h".to_vec()]);
        assert_eq!(partial.cache_tree().unwrap().root.entry_count(), Some(3));
    }
}
