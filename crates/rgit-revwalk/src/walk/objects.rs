// SPDX-License-Identifier: MIT
//! Listing the trees and blobs of a walk (`rev-list --objects`).
//!
//! Written from `Documentation/rev-list-options.adoc` ("Object Traversal")
//! and the observable output of `git rev-list --objects`:
//!
//! - every object is listed once, with a name: the path of a tree or blob
//!   below the commit's root tree (the root tree itself has an empty
//!   name), the tag name for a tag;
//! - a tree is listed before its entries, which follow in tree order, each
//!   subtree directly followed by its own entries;
//! - excluded objects are not listed: the trees of the excluded parents of
//!   listed commits ("edges") and excluded tree or blob arguments, with
//!   everything below them;
//! - submodule entries (gitlinks) are not followed.

use std::collections::HashSet;

use rgit_hash::ObjectId;

/// The kind of a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A subtree.
    Tree,
    /// A blob (file or symlink).
    Blob,
    /// A gitlink (submodule commit), not followed.
    Gitlink,
}

/// One entry of a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeItem {
    /// Entry kind.
    pub kind: ItemKind,
    /// Entry name.
    pub name: Vec<u8>,
    /// Object id.
    pub id: ObjectId,
}

/// Supplies trees to the lister.
pub trait TreeSource {
    /// Error for missing or unreadable trees.
    type Error;
    /// The entries of tree `id`, in tree order.
    fn tree(&mut self, id: &ObjectId) -> Result<Vec<TreeItem>, Self::Error>;
}

impl<S: TreeSource + ?Sized> TreeSource for &mut S {
    type Error = S::Error;
    fn tree(&mut self, id: &ObjectId) -> Result<Vec<TreeItem>, Self::Error> {
        (**self).tree(id)
    }
}

/// Lists objects once each, skipping excluded ones.
#[derive(Debug)]
pub struct ObjectLister<S: TreeSource> {
    source: S,
    seen: HashSet<ObjectId>,
    excluded: HashSet<ObjectId>,
}

impl<S: TreeSource> ObjectLister<S> {
    /// A lister reading trees from `source`.
    pub fn new(source: S) -> Self {
        ObjectLister {
            source,
            seen: HashSet::new(),
            excluded: HashSet::new(),
        }
    }

    /// Exclude a single object (a blob or a tag).
    pub fn exclude(&mut self, id: &ObjectId) {
        self.excluded.insert(*id);
    }

    /// Exclude tree `id` and everything below it.
    pub fn exclude_tree(&mut self, id: &ObjectId) -> Result<(), S::Error> {
        let mut stack = vec![*id];
        while let Some(t) = stack.pop() {
            if !self.excluded.insert(t) {
                continue;
            }
            for item in self.source.tree(&t)? {
                match item.kind {
                    ItemKind::Tree => stack.push(item.id),
                    ItemKind::Blob => {
                        self.excluded.insert(item.id);
                    }
                    ItemKind::Gitlink => {}
                }
            }
        }
        Ok(())
    }

    /// Whether `id` is excluded.
    pub fn is_excluded(&self, id: &ObjectId) -> bool {
        self.excluded.contains(id)
    }

    /// Take a single object (blob, tag or commit) once: `true` when it is
    /// neither excluded nor listed yet, and from now on listed.
    pub fn take(&mut self, id: &ObjectId) -> bool {
        !self.excluded.contains(id) && self.seen.insert(*id)
    }

    /// List tree `id` named `name` and everything below it that is not
    /// listed or excluded yet, calling `out` for each object.
    pub fn list_tree(
        &mut self,
        id: &ObjectId,
        name: &[u8],
        out: &mut dyn FnMut(&ObjectId, &[u8]),
    ) -> Result<(), S::Error> {
        if !self.take(id) {
            return Ok(());
        }
        out(id, name);
        let items = self.source.tree(id)?;
        for item in items {
            let path = if name.is_empty() {
                item.name.clone()
            } else {
                let mut p = name.to_vec();
                p.push(b'/');
                p.extend_from_slice(&item.name);
                p
            };
            match item.kind {
                ItemKind::Tree => self.list_tree(&item.id, &path, out)?,
                ItemKind::Blob => {
                    if self.take(&item.id) {
                        out(&item.id, &path);
                    }
                }
                ItemKind::Gitlink => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rgit_hash::HashKind;

    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    struct Trees(HashMap<ObjectId, Vec<TreeItem>>);

    impl TreeSource for Trees {
        type Error = ObjectId;
        fn tree(&mut self, t: &ObjectId) -> Result<Vec<TreeItem>, ObjectId> {
            self.0.get(t).cloned().ok_or(*t)
        }
    }

    fn item(kind: ItemKind, name: &str, n: u8) -> TreeItem {
        TreeItem {
            kind,
            name: name.as_bytes().to_vec(),
            id: id(n),
        }
    }

    #[test]
    fn lists_once_in_tree_order_and_skips_excluded() {
        let mut t = HashMap::new();
        t.insert(
            id(1),
            vec![
                item(ItemKind::Blob, "a", 10),
                item(ItemKind::Tree, "d", 2),
                item(ItemKind::Gitlink, "sub", 99),
                item(ItemKind::Blob, "z", 11),
            ],
        );
        t.insert(id(2), vec![item(ItemKind::Blob, "b", 12)]);
        t.insert(id(3), vec![item(ItemKind::Blob, "old", 11)]);
        let mut l = ObjectLister::new(Trees(t));
        l.exclude_tree(&id(3)).unwrap();
        let mut got = Vec::new();
        l.list_tree(&id(1), b"", &mut |i, n| {
            got.push((i.as_bytes()[0], n.to_vec()))
        })
        .unwrap();
        l.list_tree(&id(1), b"", &mut |i, n| {
            got.push((i.as_bytes()[0], n.to_vec()))
        })
        .unwrap();
        let want: Vec<(u8, Vec<u8>)> = vec![
            (1, b"".to_vec()),
            (10, b"a".to_vec()),
            (2, b"d".to_vec()),
            (12, b"d/b".to_vec()),
        ];
        assert_eq!(got, want);
        assert!(l.is_excluded(&id(11)));
    }
}
