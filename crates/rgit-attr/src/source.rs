// SPDX-License-Identifier: MIT

//! Where per-directory `.gitattributes` files come from: the work tree,
//! the index or a tree, and reading the other attributes files.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rgit_hash::ObjectId;
use rgit_index::{Index, Stage};
use rgit_object::{ObjectKind, Tree, TreeEntry};

use crate::{MAX_FILE_SIZE, messages};

/// Object access for the index and tree sources.
pub trait ObjectReader {
    /// The content of the blob `id`; `None` when it is missing or not a
    /// blob.
    fn blob(&self, id: &ObjectId) -> Option<Vec<u8>>;

    /// The entries of the tree `id`; `None` when it is missing or not a
    /// tree.
    fn tree(&self, id: &ObjectId) -> Option<Vec<TreeEntry>>;
}

impl ObjectReader for rgit_odb::ObjectDatabase {
    fn blob(&self, id: &ObjectId) -> Option<Vec<u8>> {
        match self.read(id) {
            Ok(Some(raw)) if raw.kind == ObjectKind::Blob => Some(raw.data),
            _ => None,
        }
    }

    fn tree(&self, id: &ObjectId) -> Option<Vec<TreeEntry>> {
        match self.read(id) {
            Ok(Some(raw)) if raw.kind == ObjectKind::Tree => {
                Tree::parse(id.kind(), &raw.data).ok().map(|t| t.entries)
            }
            _ => None,
        }
    }
}

/// Which copy of a `.gitattributes` file wins when both the work tree and
/// the index have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The work tree file; the index entry only when the file is missing.
    /// What git uses for everything except checkout.
    WorkTreeFirst,
    /// The index entry; the work tree file only when there is no entry.
    /// What git uses while checking files out.
    IndexFirst,
}

/// The source of the per-directory `.gitattributes` files.
pub enum Source<'a> {
    /// None at all (a bare repository without a tree to read from).
    None,
    /// The work tree at `root`, with `index` (and the objects to read its
    /// blobs) as the other source in `order`.
    WorkTree {
        /// The top of the work tree.
        root: PathBuf,
        /// The index and the object reader for its blobs.
        index: Option<(&'a Index, &'a dyn ObjectReader)>,
        /// Which copy wins.
        order: Order,
    },
    /// The index only (`check-attr --cached`).
    Index(&'a Index, &'a dyn ObjectReader),
    /// The tree `tree` (`--source`, `--attr-source`, `attr.tree`); `None`
    /// is the empty tree.
    Tree(Option<ObjectId>, &'a dyn ObjectReader),
}

impl std::fmt::Debug for Source<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::None => f.write_str("None"),
            Source::WorkTree { root, order, index } => f
                .debug_struct("WorkTree")
                .field("root", root)
                .field("index", &index.is_some())
                .field("order", order)
                .finish(),
            Source::Index(..) => f.write_str("Index"),
            Source::Tree(t, _) => f.debug_tuple("Tree").field(t).finish(),
        }
    }
}

/// The result of reading one copy of a file.
enum Read1 {
    /// The contents (possibly empty: a directory, a file that is too large).
    Data(Option<Vec<u8>>),
    /// Not there; the other copy may be used.
    Missing,
}

impl Source<'_> {
    /// The contents of `rel` (`<dir>.gitattributes`, relative to the top
    /// of the work tree); messages go to `warnings`.
    pub(crate) fn read(&self, rel: &[u8], warnings: &mut Vec<String>) -> Option<Vec<u8>> {
        let shown = String::from_utf8_lossy(rel).into_owned();
        match self {
            Source::None => None,
            Source::WorkTree { root, index, order } => {
                let read = |from_index: bool, w: &mut Vec<String>| match (from_index, index) {
                    (false, _) => read_work_tree(root, rel, &shown, w),
                    (true, Some((i, o))) => read_index(i, *o, rel, &shown, w),
                    (true, None) => Read1::Missing,
                };
                let index_first = *order == Order::IndexFirst;
                match read(index_first, warnings) {
                    Read1::Data(d) => d,
                    Read1::Missing => match read(!index_first, warnings) {
                        Read1::Data(d) => d,
                        Read1::Missing => None,
                    },
                }
            }
            Source::Index(i, o) => match read_index(i, *o, rel, &shown, warnings) {
                Read1::Data(d) => d,
                Read1::Missing => None,
            },
            Source::Tree(tree, o) => {
                let id = tree.as_ref()?;
                read_tree(*o, id, rel, &shown, warnings)
            }
        }
    }
}

/// A blob, unless it is too large.
fn checked_blob(data: Vec<u8>, shown: &str, warnings: &mut Vec<String>) -> Option<Vec<u8>> {
    if data.len() as u64 > MAX_FILE_SIZE {
        warnings.push(messages::blob_too_large(shown));
        None
    } else {
        Some(data)
    }
}

fn read_index(
    index: &Index,
    objects: &dyn ObjectReader,
    rel: &[u8],
    shown: &str,
    warnings: &mut Vec<String>,
) -> Read1 {
    let Some(entry) = index.get(rel, Stage::MERGED) else {
        return Read1::Missing;
    };
    if !entry.mode.is_file() {
        return Read1::Missing;
    }
    Read1::Data(
        objects
            .blob(&entry.id)
            .and_then(|d| checked_blob(d, shown, warnings)),
    )
}

fn read_tree(
    objects: &dyn ObjectReader,
    root: &ObjectId,
    rel: &[u8],
    shown: &str,
    warnings: &mut Vec<String>,
) -> Option<Vec<u8>> {
    let mut id = *root;
    let mut parts = rel.split(|&b| b == b'/').peekable();
    while let Some(name) = parts.next() {
        let entries = objects.tree(&id)?;
        let e = entries.into_iter().find(|e| e.name == name)?;
        let last = parts.peek().is_none();
        let mode = e.mode.canonical();
        if last {
            if !matches!(mode.bits(), 0o100644 | 0o100755) {
                return None;
            }
            return objects
                .blob(&e.oid)
                .and_then(|d| checked_blob(d, shown, warnings));
        }
        if !mode.is_tree() {
            return None;
        }
        id = e.oid;
    }
    None
}

/// Errors that need no warning: the file is not there.
fn is_missing(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Reads an in-tree file without following a symbolic link at the file
/// itself. A missing or unreadable file lets the index copy be used.
fn read_work_tree(root: &Path, rel: &[u8], shown: &str, warnings: &mut Vec<String>) -> Read1 {
    let path = crate::os::join(root, rel);
    let mut file = match crate::os::open_read(&path, true) {
        Ok(f) => f,
        Err(e) => {
            if !is_missing(&e) {
                warnings.push(messages::unable_to_access(shown, &crate::os::strerror(&e)));
            }
            return Read1::Missing;
        }
    };
    let Ok(meta) = file.metadata() else {
        return Read1::Data(None);
    };
    if meta.is_dir() {
        return Read1::Data(None);
    }
    if meta.len() > MAX_FILE_SIZE {
        warnings.push(messages::file_too_large(shown));
        return Read1::Data(None);
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    match file.read_to_end(&mut data) {
        Ok(_) => Read1::Data(Some(data)),
        Err(e) => {
            warnings.push(messages::unable_to_access(shown, &crate::os::strerror(&e)));
            Read1::Data(None)
        }
    }
}

/// Reads one of the files outside the work tree (system, global,
/// `info/attributes`), following symbolic links. `shown` names it in
/// messages.
pub(crate) fn read_file(path: &Path, shown: &str, warnings: &mut Vec<String>) -> Option<Vec<u8>> {
    let fail = |e: io::Error, warnings: &mut Vec<String>| {
        if !is_missing(&e) && e.kind() != io::ErrorKind::IsADirectory {
            warnings.push(messages::unable_to_access(shown, &crate::os::strerror(&e)));
        }
        None
    };
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return fail(e, warnings),
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => return fail(e, warnings),
    };
    if meta.is_dir() {
        return None;
    }
    if meta.len() > MAX_FILE_SIZE {
        warnings.push(messages::file_too_large(shown));
        return None;
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    match file.read_to_end(&mut data) {
        Ok(_) => Some(data),
        Err(e) => fail(e, warnings),
    }
}
