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
//!
//! With an object filter ([`ObjectLister::set_object_filter`], see
//! `rgit_pack::write::FilterState`) objects are shown, remembered and
//! entered as the filter decides; a tree whose entries are skipped is
//! reported to [`ObjectLister::set_skip_hook`] (git traces it).
//! A [`TreeSource`] decides what happens with missing trees and blobs
//! ([`TreeSource::tree_or_missing`], [`TreeSource::blob`]).

use std::collections::HashSet;

use rgit_hash::ObjectId;
use rgit_pack::write::FilterState;

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

    /// The entries of tree `id` found at `path` while listing; `Ok(None)`
    /// for a missing tree that is to be passed over. The default reads
    /// [`TreeSource::tree`].
    fn tree_or_missing(
        &mut self,
        id: &ObjectId,
        path: &[u8],
    ) -> Result<Option<Vec<TreeItem>>, Self::Error> {
        let _ = path;
        self.tree(id).map(Some)
    }

    /// Whether blob `id` is here, without side effects: `Some(size)`
    /// (the size if known), `None` when it is missing. The default takes
    /// every blob to be there.
    fn peek_blob(&mut self, id: &ObjectId) -> Option<Option<u64>> {
        let _ = id;
        Some(None)
    }

    /// A blob [`TreeSource::peek_blob`] found missing is to be listed at
    /// `path`: `Ok(Some(size))` when it could be had after all (fetched),
    /// `Ok(None)` for a missing blob that is to be passed over. The
    /// default takes every blob to be there.
    fn blob(
        &mut self,
        id: &ObjectId,
        path: &[u8],
        listed: bool,
    ) -> Result<Option<Option<u64>>, Self::Error> {
        let _ = (id, path, listed);
        Ok(Some(None))
    }
}

impl<S: TreeSource + ?Sized> TreeSource for &mut S {
    type Error = S::Error;
    fn tree(&mut self, id: &ObjectId) -> Result<Vec<TreeItem>, Self::Error> {
        (**self).tree(id)
    }
    fn tree_or_missing(
        &mut self,
        id: &ObjectId,
        path: &[u8],
    ) -> Result<Option<Vec<TreeItem>>, Self::Error> {
        (**self).tree_or_missing(id, path)
    }
    fn blob(
        &mut self,
        id: &ObjectId,
        path: &[u8],
        listed: bool,
    ) -> Result<Option<Option<u64>>, Self::Error> {
        (**self).blob(id, path, listed)
    }
    fn peek_blob(&mut self, id: &ObjectId) -> Option<Option<u64>> {
        (**self).peek_blob(id)
    }
}

/// Decides whether a path (and whether it is a tree) of a commit's tree
/// is listed (path limiting).
pub type PathFilter = Box<dyn Fn(&[u8], bool) -> bool>;

/// Called with the path of a tree whose entries a filter skips.
pub type SkipHook = Box<dyn FnMut(&[u8])>;

/// Lists objects once each, skipping excluded ones.
pub struct ObjectLister<S: TreeSource> {
    source: S,
    seen: HashSet<ObjectId>,
    excluded: HashSet<ObjectId>,
    filter: Option<PathFilter>,
    objects: Option<FilterState>,
    shown: HashSet<ObjectId>,
    on_skip: Option<SkipHook>,
}

impl<S: TreeSource> std::fmt::Debug for ObjectLister<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectLister")
            .field("seen", &self.seen.len())
            .field("excluded", &self.excluded.len())
            .field("filtered", &self.filter.is_some())
            .finish_non_exhaustive()
    }
}

impl<S: TreeSource> ObjectLister<S> {
    /// A lister reading trees from `source`.
    pub fn new(source: S) -> Self {
        ObjectLister {
            source,
            seen: HashSet::new(),
            excluded: HashSet::new(),
            filter: None,
            objects: None,
            shown: HashSet::new(),
            on_skip: None,
        }
    }

    /// Filter the listed trees and blobs (`--filter`).
    pub fn set_object_filter(&mut self, filter: FilterState) {
        self.objects = Some(filter);
    }

    /// The object filter, if any.
    pub fn object_filter(&self) -> Option<&FilterState> {
        self.objects.as_ref()
    }

    /// The object filter, if any, for objects listed outside of trees.
    pub fn object_filter_mut(&mut self) -> Option<&mut FilterState> {
        self.objects.as_mut()
    }

    /// Called with the path of every tree whose entries the filter skips
    /// (`<path>/` for a subtree, empty for a root tree).
    pub fn set_skip_hook(&mut self, hook: SkipHook) {
        self.on_skip = Some(hook);
    }

    /// Whether an object was listed by the filtered walk.
    pub fn was_shown(&self, id: &ObjectId) -> bool {
        self.shown.contains(id)
    }

    /// The source of the trees.
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }

    /// The objects the filter left out and did not show elsewhere.
    pub fn omitted(&self) -> Vec<ObjectId> {
        let Some(f) = &self.objects else {
            return Vec::new();
        };
        let mut out: Vec<ObjectId> = f
            .omitted()
            .into_iter()
            .filter(|id| !self.shown.contains(id) && !self.excluded.contains(id))
            .collect();
        out.sort();
        out
    }

    /// Path limiting for [`ObjectLister::list_commit_tree`]: only entries
    /// the filter accepts (directories leading to accepted paths) are
    /// listed.
    pub fn set_filter(&mut self, filter: PathFilter) {
        self.filter = Some(filter);
    }

    /// List the root tree `id` of a commit, honouring the path filter.
    pub fn list_commit_tree(
        &mut self,
        id: &ObjectId,
        out: &mut dyn FnMut(&ObjectId, &[u8]),
    ) -> Result<(), S::Error> {
        self.list(id, b"", out, true, 0, false)
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
        self.list(id, name, out, false, 0, false)
    }

    /// [`ObjectLister::list_tree`] for a tree named on the command line:
    /// with `provided`, it is shown whatever the object filter says about
    /// it (its entries are filtered).
    pub fn list_provided_tree(
        &mut self,
        id: &ObjectId,
        name: &[u8],
        out: &mut dyn FnMut(&ObjectId, &[u8]),
        provided: bool,
    ) -> Result<(), S::Error> {
        self.list(id, name, out, false, 0, provided)
    }

    /// A blob named on the command line: listed unless `filter_it` and
    /// the object filter leaves it out.
    pub fn list_provided_blob(
        &mut self,
        id: &ObjectId,
        name: &[u8],
        out: &mut dyn FnMut(&ObjectId, &[u8]),
        filter_it: bool,
    ) -> Result<(), S::Error> {
        if filter_it && self.objects.is_some() {
            return self.list_blob(id, name, 0, out);
        }
        if self.take(id) {
            self.shown.insert(*id);
            out(id, name);
        }
        Ok(())
    }

    /// List the blob `id` found at `path`, `depth` levels below a root.
    fn list_blob(
        &mut self,
        id: &ObjectId,
        path: &[u8],
        depth: u64,
        out: &mut dyn FnMut(&ObjectId, &[u8]),
    ) -> Result<(), S::Error> {
        if self.excluded.contains(id) || self.seen.contains(id) {
            return Ok(());
        }
        let peek = self.source.peek_blob(id);
        let Some(f) = self.objects.as_mut() else {
            self.seen.insert(*id);
            if peek.is_some() || self.source.blob(id, path, true)?.is_some() {
                out(id, path);
            }
            return Ok(());
        };
        // A missing blob the filter leaves out is not missed.
        let v = f.blob(id, path, depth, peek.flatten());
        if v.mark_seen {
            self.seen.insert(*id);
        }
        if v.show {
            if peek.is_none() && self.source.blob(id, path, true)?.is_none() {
                self.seen.insert(*id);
                return Ok(());
            }
            if self.shown.insert(*id) {
                out(id, path);
            }
        }
        Ok(())
    }

    fn list(
        &mut self,
        id: &ObjectId,
        name: &[u8],
        out: &mut dyn FnMut(&ObjectId, &[u8]),
        filtered: bool,
        depth: u64,
        provided: bool,
    ) -> Result<(), S::Error> {
        let mut entered = false;
        let show;
        if provided && self.objects.is_some() {
            // A tree named on the command line is listed; only its
            // entries are filtered.
            if !self.take(id) {
                return Ok(());
            }
            show = true;
        } else if let Some(f) = self.objects.as_mut() {
            if self.excluded.contains(id) || self.seen.contains(id) {
                return Ok(());
            }
            let v = f.begin_tree(id, name, depth);
            if v.mark_seen {
                self.seen.insert(*id);
            }
            if v.skip {
                if v.show && self.shown.insert(*id) {
                    out(id, name);
                }
                if let Some(hook) = &mut self.on_skip {
                    let mut p = name.to_vec();
                    if !p.is_empty() {
                        p.push(b'/');
                    }
                    hook(&p);
                }
                return Ok(());
            }
            entered = true;
            show = v.show;
        } else {
            if !self.take(id) {
                return Ok(());
            }
            show = true;
        }
        // A missing tree is not listed.
        let Some(items) = self.source.tree_or_missing(id, name)? else {
            if entered {
                if let Some(f) = self.objects.as_mut() {
                    f.end_tree();
                }
            }
            return Ok(());
        };
        if show && (self.objects.is_none() || self.shown.insert(*id)) {
            out(id, name);
        }
        // The filter does not see a tree named on the command line: its
        // entries count as found at the top (as observed from git).
        let child_depth = if provided && self.objects.is_some() {
            depth
        } else {
            depth + 1
        };
        for item in items {
            let path = if name.is_empty() {
                item.name.clone()
            } else {
                let mut p = name.to_vec();
                p.push(b'/');
                p.extend_from_slice(&item.name);
                p
            };
            if filtered
                && let Some(f) = &self.filter
                && !f(&path, item.kind == ItemKind::Tree)
            {
                continue;
            }
            match item.kind {
                ItemKind::Tree => self.list(&item.id, &path, out, filtered, child_depth, false)?,
                ItemKind::Blob => self.list_blob(&item.id, &path, child_depth, out)?,
                ItemKind::Gitlink => {}
            }
        }
        if entered {
            if let Some(f) = self.objects.as_mut() {
                f.end_tree();
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
