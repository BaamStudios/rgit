// SPDX-License-Identifier: MIT

//! Reading trees and blobs, and listing a tree as index entries.

use rgit_hash::{HashKind, ObjectId};
use rgit_index::{Entry, Mode, Stage};
use rgit_object::{ObjectKind, RawObject, TreeIter};
use rgit_odb::ObjectDatabase;

use crate::Error;

/// Where the engine reads objects from. Implemented for
/// [`ObjectDatabase`]; tests may provide their own.
pub trait Objects {
    /// The hash algorithm of the repository.
    fn hash_kind(&self) -> HashKind;
    /// An object, `Ok(None)` when it does not exist. The error is the
    /// message to print.
    fn read_raw(&self, id: &ObjectId) -> Result<Option<RawObject>, String>;
    /// Whether the object exists (used to validate cache tree nodes).
    fn has(&self, id: &ObjectId) -> bool;
    /// Fetch the missing ones of `ids` in one batch (partial clone)
    /// before they are read one by one. The default does nothing.
    fn prefetch(&self, ids: &[ObjectId]) {
        let _ = ids;
    }
}

impl Objects for ObjectDatabase {
    fn hash_kind(&self) -> HashKind {
        ObjectDatabase::hash_kind(self)
    }

    fn read_raw(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
        self.read(id).map_err(|e| e.to_string())
    }

    fn has(&self, id: &ObjectId) -> bool {
        self.contains(id)
    }

    fn prefetch(&self, ids: &[ObjectId]) {
        ObjectDatabase::prefetch(self, ids);
    }
}

/// The content of blob `id`.
///
/// # Errors
/// [`Error::MissingBlob`] when it cannot be read.
pub fn read_blob(odb: &dyn Objects, id: &ObjectId) -> Result<Vec<u8>, Error> {
    match odb.read_raw(id) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Blob => Ok(raw.data),
        _ => Err(Error::MissingBlob(*id)),
    }
}

/// All non-tree entries of tree `id`, recursively, as stage-0 index
/// entries without stat data, in index order. Modes are canonicalised as
/// git does when reading a tree.
///
/// # Errors
/// [`Error::MissingTree`] for a tree that cannot be read,
/// [`Error::MissingSubtree`] for one of its subtrees,
/// [`Error::CorruptTree`] for one that cannot be parsed.
pub fn tree_entries(odb: &dyn Objects, id: &ObjectId) -> Result<Vec<Entry>, Error> {
    let mut out = Vec::new();
    walk(odb, id, &mut Vec::new(), &mut out)?;
    out.sort_by(|a, b| a.path().cmp(b.path()));
    Ok(out)
}

fn walk(
    odb: &dyn Objects,
    id: &ObjectId,
    base: &mut Vec<u8>,
    out: &mut Vec<Entry>,
) -> Result<(), Error> {
    let raw = match odb.read_raw(id) {
        Ok(Some(raw)) if raw.kind == ObjectKind::Tree => raw,
        _ => return Err(Error::MissingTree(*id)),
    };
    // A broken tree can name an entry twice as a file (or submodule) and
    // as a directory; git's tree walk sees only the first of them. (Two
    // files of one name both reach the index, where git reports them.)
    let mut seen = std::collections::HashMap::new();
    for item in TreeIter::new(odb.hash_kind(), &raw.data) {
        let e = item.map_err(|e| Error::CorruptTree(e.to_string()))?;
        let is_tree = rgit_object::EntryMode::from_bits(e.mode_bits).is_tree();
        if *seen.entry(e.name).or_insert(is_tree) != is_tree {
            continue;
        }
        let len = base.len();
        base.extend_from_slice(e.name);
        let mode = rgit_object::EntryMode::from_bits(e.mode_bits).canonical();
        if mode.is_tree() {
            base.push(b'/');
            walk(odb, &e.oid, base, out).map_err(|err| match err {
                Error::MissingTree(id) => Error::MissingSubtree(id),
                other => other,
            })?;
        } else {
            out.push(Entry::new(
                base.clone(),
                Stage::MERGED,
                Mode(mode.bits()),
                e.oid,
            ));
        }
        base.truncate(len);
    }
    Ok(())
}
