// SPDX-License-Identifier: MIT
//! The tree of a branch in memory: loaded lazily from tree objects,
//! changed by `filemodify`, `filedelete`, `filecopy`, `filerename` and
//! `filedeleteall`, and written back as tree objects for each commit.
//! Directories that become empty disappear.

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{EntryMode, ObjectKind, Tree as TreeObject, TreeEntry, TreeIter};

use crate::Die;

pub const MODE_TREE: u32 = 0o040000;
pub const MODE_GITLINK: u32 = 0o160000;

/// Reads tree objects for lazy loading.
pub trait Loader {
    fn load_tree(&mut self, oid: &ObjectId) -> Result<Vec<u8>, Die>;
}

/// Writes tree objects.
pub trait Writer {
    /// Write `data`; `prev` is the previous version of the directory (the
    /// delta base).
    fn write_tree(&mut self, data: &[u8], prev: Option<ObjectId>) -> Result<ObjectId, Die>;
}

#[derive(Debug, Clone, Default)]
pub struct Tree {
    pub entries: Vec<Entry>,
    /// The tree object, while nothing changed since it was read or written.
    pub oid: Option<ObjectId>,
    /// The tree object last read or written for this directory.
    pub prev: Option<ObjectId>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: Vec<u8>,
    pub mode: u32,
    /// For directories: the tree object when the directory is unchanged.
    pub oid: ObjectId,
    /// The loaded contents of a directory.
    pub sub: Option<Box<Tree>>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.mode == MODE_TREE
    }
}

impl Tree {
    /// A tree to be read from `oid` when needed.
    pub fn from_entries(hash: HashKind, data: &[u8], oid: ObjectId) -> Result<Tree, Die> {
        let mut entries = Vec::new();
        for e in TreeIter::new(hash, data) {
            let e = e.map_err(|_| Die::Fatal(format!("corrupt tree {oid}")))?;
            entries.push(Entry {
                name: e.name.to_vec(),
                mode: e.mode_bits,
                oid: e.oid,
                sub: None,
            });
        }
        Ok(Tree {
            entries,
            oid: Some(oid),
            prev: Some(oid),
        })
    }

    pub fn load(hash: HashKind, oid: &ObjectId, loader: &mut dyn Loader) -> Result<Tree, Die> {
        let data = loader.load_tree(oid)?;
        Tree::from_entries(hash, &data, *oid)
    }

    fn find(&self, name: &[u8]) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }

    /// Whether the tree has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Load the contents of the directory entry `e`.
fn ensure_loaded(hash: HashKind, e: &mut Entry, loader: &mut dyn Loader) -> Result<(), Die> {
    if e.sub.is_none() {
        e.sub = Some(Box::new(Tree::load(hash, &e.oid, loader)?));
    }
    Ok(())
}

/// What a path names.
#[derive(Debug, Clone)]
pub struct Found {
    pub mode: u32,
    pub oid: ObjectId,
    pub sub: Option<Box<Tree>>,
}

/// Set `path` (components, not empty) to `value`. Directories on the way
/// are created; files on the way become directories.
pub fn set(
    hash: HashKind,
    root: &mut Tree,
    path: &[&[u8]],
    value: Found,
    loader: &mut dyn Loader,
) -> Result<(), Die> {
    root.oid = None;
    let (first, rest) = path.split_first().expect("non-empty path");
    let pos = root.find(first);
    if rest.is_empty() {
        let entry = Entry {
            name: first.to_vec(),
            mode: value.mode,
            oid: value.oid,
            sub: value.sub,
        };
        match pos {
            Some(i) => root.entries[i] = entry,
            None => root.entries.push(entry),
        }
        return Ok(());
    }
    let i = match pos {
        Some(i) if root.entries[i].is_dir() => i,
        Some(i) => {
            root.entries[i] = Entry {
                name: first.to_vec(),
                mode: MODE_TREE,
                oid: ObjectId::null(hash),
                sub: Some(Box::default()),
            };
            i
        }
        None => {
            root.entries.push(Entry {
                name: first.to_vec(),
                mode: MODE_TREE,
                oid: ObjectId::null(hash),
                sub: Some(Box::default()),
            });
            root.entries.len() - 1
        }
    };
    let e = &mut root.entries[i];
    ensure_loaded(hash, e, loader)?;
    set(hash, e.sub.as_mut().expect("loaded"), rest, value, loader)
}

/// Remove `path` (components, not empty). Directories that become empty
/// are removed. Returns whether something was removed.
pub fn delete(
    hash: HashKind,
    root: &mut Tree,
    path: &[&[u8]],
    loader: &mut dyn Loader,
) -> Result<bool, Die> {
    let (first, rest) = path.split_first().expect("non-empty path");
    let Some(i) = root.find(first) else {
        return Ok(false);
    };
    if rest.is_empty() {
        root.entries.remove(i);
        root.oid = None;
        return Ok(true);
    }
    let e = &mut root.entries[i];
    if !e.is_dir() {
        return Ok(false);
    }
    ensure_loaded(hash, e, loader)?;
    let sub = e.sub.as_mut().expect("loaded");
    if !delete(hash, sub, rest, loader)? {
        return Ok(false);
    }
    root.oid = None;
    if sub.is_empty() {
        root.entries.remove(i);
    }
    Ok(true)
}

/// What `path` names; `None` when nothing (an empty directory counts as
/// nothing).
pub fn get(
    hash: HashKind,
    root: &mut Tree,
    path: &[&[u8]],
    loader: &mut dyn Loader,
) -> Result<Option<Found>, Die> {
    let (first, rest) = path.split_first().expect("non-empty path");
    let Some(i) = root.find(first) else {
        return Ok(None);
    };
    let e = &mut root.entries[i];
    if rest.is_empty() {
        if e.is_dir() {
            if let Some(sub) = &e.sub {
                if sub.is_empty() {
                    return Ok(None);
                }
            }
        }
        return Ok(Some(Found {
            mode: e.mode,
            oid: e.oid,
            sub: e.sub.clone(),
        }));
    }
    if !e.is_dir() {
        return Ok(None);
    }
    ensure_loaded(hash, e, loader)?;
    get(hash, e.sub.as_mut().expect("loaded"), rest, loader)
}

/// Write `tree` and its changed subdirectories; returns its id.
pub fn write(tree: &mut Tree, writer: &mut dyn Writer) -> Result<ObjectId, Die> {
    if let Some(oid) = tree.oid {
        return Ok(oid);
    }
    let mut obj = TreeObject::new();
    for e in &mut tree.entries {
        if let Some(sub) = &mut e.sub {
            if sub.is_empty() {
                continue;
            }
            e.oid = write(sub, writer)?;
        }
        obj.entries
            .push(TreeEntry::new(EntryMode::from_bits(e.mode), e.name.clone(), e.oid));
    }
    obj.entries.sort_by(TreeEntry::cmp_tree_order);
    let oid = writer.write_tree(&obj.serialize(), tree.prev)?;
    tree.oid = Some(oid);
    tree.prev = Some(oid);
    Ok(oid)
}

/// The type of objects entries with `mode` name.
pub fn mode_kind(mode: u32) -> ObjectKind {
    match mode {
        MODE_TREE => ObjectKind::Tree,
        MODE_GITLINK => ObjectKind::Commit,
        _ => ObjectKind::Blob,
    }
}

/// All entries below `tree` as `(path, mode, oid)` (directories are
/// descended into), loading what is needed.
pub fn walk(
    hash: HashKind,
    tree: &mut Tree,
    prefix: &mut Vec<u8>,
    loader: &mut dyn Loader,
    f: &mut dyn FnMut(&[u8], u32, &ObjectId),
) -> Result<(), Die> {
    for e in &mut tree.entries {
        let len = prefix.len();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(&e.name);
        if e.is_dir() {
            ensure_loaded(hash, e, loader)?;
            walk(hash, e.sub.as_mut().expect("loaded"), prefix, loader, f)?;
        } else {
            f(prefix, e.mode, &e.oid);
        }
        prefix.truncate(len);
    }
    Ok(())
}
