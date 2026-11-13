// SPDX-License-Identifier: MIT
//! Tree objects from an index (Documentation/git-write-tree.adoc).

use rgit_hash::ObjectId;
use rgit_index::{CacheTree, Index, TreeNode};
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};
use rgit_odb::ObjectDatabase;

/// Options of [`write_tree`].
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteTreeOptions<'a> {
    /// Do not check that the blobs the index names exist (`--missing-ok`).
    pub missing_ok: bool,
    /// Write only the tree of this directory (`--prefix=<dir>/`).
    pub prefix: Option<&'a str>,
}

/// Why no tree was written.
#[derive(Debug)]
pub enum WriteTreeError {
    /// The index has conflict entries; the paths in index order, each once.
    Unmerged(Vec<Vec<u8>>),
    /// An entry names an object that does not exist.
    MissingObject {
        mode: u32,
        id: ObjectId,
        path: Vec<u8>,
    },
    /// No entry lies below the prefix.
    PrefixNotFound(String),
    /// An entry has a mode trees cannot hold.
    BadMode,
    /// Writing an object failed.
    Odb(rgit_odb::Error),
}

impl std::fmt::Display for WriteTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteTreeError::Odb(e) => e.fmt(f),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for WriteTreeError {}

type Item<'a> = (&'a [u8], u32, ObjectId);

/// Write the trees for `index` (or its `prefix` subdirectory) and return
/// the top tree's id. Intent-to-add entries are left out; gitlinks are not
/// checked for existence.
pub fn write_tree(
    odb: &ObjectDatabase,
    index: &Index,
    opts: &WriteTreeOptions<'_>,
) -> Result<ObjectId, WriteTreeError> {
    write_tree_cached(odb, index, opts).map(|(id, _)| id)
}

/// [`write_tree`], also returning the cache tree (`TREE` extension) that
/// describes the written trees. There is none for a `prefix` or when the
/// index has intent-to-add entries (their directories have no valid tree).
pub fn write_tree_cached(
    odb: &ObjectDatabase,
    index: &Index,
    opts: &WriteTreeOptions<'_>,
) -> Result<(ObjectId, Option<CacheTree>), WriteTreeError> {
    let has_ita = index.entries().iter().any(|e| e.flags.intent_to_add);
    let mut items: Vec<Item<'_>> = Vec::new();
    let mut unmerged: Vec<Vec<u8>> = Vec::new();
    for e in index.entries() {
        if e.stage().get() != 0 {
            if unmerged.last().map(Vec::as_slice) != Some(e.path()) {
                unmerged.push(e.path().to_vec());
            }
            continue;
        }
        if e.flags.intent_to_add {
            continue;
        }
        items.push((e.path(), e.mode.0, e.id));
    }
    if !unmerged.is_empty() {
        return Err(WriteTreeError::Unmerged(unmerged));
    }
    if !opts.missing_ok {
        for (path, mode, id) in &items {
            if *mode & 0o170000 == 0o160000 {
                continue;
            }
            if !matches!(odb.header(id), Ok(Some(_))) {
                return Err(WriteTreeError::MissingObject {
                    mode: *mode,
                    id: *id,
                    path: path.to_vec(),
                });
            }
        }
    }
    if let Some(p) = opts.prefix {
        let dir = p.trim_end_matches('/');
        if !dir.is_empty() {
            let want = format!("{dir}/");
            let sub: Vec<Item<'_>> = items
                .iter()
                .filter(|it| it.0.starts_with(want.as_bytes()))
                .copied()
                .collect();
            if sub.is_empty() {
                return Err(WriteTreeError::PrefixNotFound(p.to_owned()));
            }
            return build(odb, &sub, want.len(), b"").map(|(id, _)| (id, None));
        }
    }
    let (id, root) = build(odb, &items, 0, b"")?;
    Ok((id, (!has_ita).then_some(CacheTree { root })))
}

/// Build the tree for `items` (in index order), whose paths all share the
/// first `strip` bytes.
fn build(
    odb: &ObjectDatabase,
    items: &[Item<'_>],
    strip: usize,
    name: &[u8],
) -> Result<(ObjectId, TreeNode), WriteTreeError> {
    let mut tree = Tree::new();
    let mut children = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let rest = &items[i].0[strip..];
        match rest.iter().position(|&c| c == b'/') {
            None => {
                let mode = EntryMode::from_octal(format!("{:o}", items[i].1).as_bytes())
                    .ok_or(WriteTreeError::BadMode)?;
                tree.entries.push(TreeEntry::new(mode, rest, items[i].2));
                i += 1;
            }
            Some(slash) => {
                let name = &rest[..=slash];
                let mut j = i + 1;
                while j < items.len() && items[j].0[strip..].starts_with(name) {
                    j += 1;
                }
                let (oid, node) = build(odb, &items[i..j], strip + slash + 1, &rest[..slash])?;
                children.push(node);
                let mode = EntryMode::from_octal(b"40000").ok_or(WriteTreeError::BadMode)?;
                tree.entries.push(TreeEntry::new(mode, &rest[..slash], oid));
                i = j;
            }
        }
    }
    tree.entries.sort_by(TreeEntry::cmp_tree_order);
    let id = odb
        .write(ObjectKind::Tree, &tree.serialize())
        .map_err(WriteTreeError::Odb)?;
    // Subtrees are kept ordered by name length, then name.
    children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    let count = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let mut node = TreeNode::new_valid(name.to_vec(), count, id);
    node.children = children;
    Ok((id, node))
}
