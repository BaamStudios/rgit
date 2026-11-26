// SPDX-License-Identifier: MIT

//! Applying an [`ObjectFilter`] during an object walk, object by object
//! (`rev-list --objects --filter`, `pack-objects --filter`).
//!
//! Written from `Documentation/rev-list-options.adoc` (`--filter`,
//! `--filter-print-omitted`, `--filter-provided-objects`) and the output
//! of `git rev-list` 2.56. The walker asks a [`FilterState`] about every
//! object it reaches and gets a [`Verdict`]: whether to show the object,
//! whether to remember it as seen (so a later visit is skipped) and, for a
//! tree, whether to skip its entries.
//!
//! - `blob:none`, `blob:limit=<n>`: blobs (of at least `n` bytes) are
//!   omitted; trees are walked once.
//! - `tree:<depth>`: trees and blobs at `depth` or deeper are omitted (a
//!   commit's root tree has depth 0). An object counts at the smallest
//!   depth it is found at: an omitted object found again higher up is
//!   shown then, and a tree is walked again when found higher up than
//!   before. The entries of an omitted tree are skipped, unless omitted
//!   objects are collected and the tree is new.
//! - `object:type=<type>`: only objects of that type are shown; nothing
//!   counts as omitted.
//! - `sparse:oid=<blob>`: a blob is shown when the sparse-checkout
//!   patterns include its path (the last matching pattern decides, a path
//!   no pattern matches inherits the decision of its directory; the root
//!   is excluded).
//! - `combine:`: an object is shown when every filter shows it, a tree
//!   skipped when every filter skips it.
//!
//! Omitted objects are collected per filter; an object shown later (found
//! again at a place where it passes) is not omitted, so the caller reports
//! [`FilterState::omitted`] minus what it showed.

use std::collections::{HashMap, HashSet};

use rgit_hash::ObjectId;

use super::ObjectFilter;
use crate::ObjectKind;

/// The sparse-checkout patterns of a `sparse:oid=` filter.
pub trait SparsePatterns {
    /// Whether the patterns include (`Some(true)`) or exclude
    /// (`Some(false)`) `path`, or say nothing about it (`None`).
    fn decide(&self, path: &[u8], is_dir: bool) -> Option<bool>;
}

/// Reads the patterns of `sparse:oid=<blob-ish>` (the argument).
pub type SparseLoader<'a, E> = dyn FnMut(&str) -> Result<Box<dyn SparsePatterns>, E> + 'a;

/// What the walker does with an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// List the object.
    pub show: bool,
    /// Remember the object as seen: later visits are skipped.
    pub mark_seen: bool,
    /// For a tree: do not walk its entries.
    pub skip: bool,
}

impl Verdict {
    const SHOW: Verdict = Verdict {
        show: true,
        mark_seen: true,
        skip: false,
    };
}

enum Node {
    BlobLimit(u64),
    Tree {
        limit: u64,
        /// The smallest depth each object was found at.
        depths: HashMap<ObjectId, u64>,
    },
    Type(ObjectKind),
    Sparse {
        patterns: Box<dyn SparsePatterns>,
        /// The inherited decision of the trees being walked.
        stack: Vec<bool>,
        /// Trees walked, with the decision they were walked under.
        walked: HashSet<(ObjectId, bool)>,
    },
    Combine {
        subs: Vec<FilterState>,
        /// Per filter: how many of the trees being walked it skipped (it
        /// is not asked about their entries).
        skipping: Vec<u32>,
    },
}

const HIDDEN: Verdict = Verdict {
    show: false,
    mark_seen: false,
    skip: true,
};

/// The state of one filter during a walk. See the module documentation.
pub struct FilterState {
    node: Node,
    collect: bool,
    omitted: HashSet<ObjectId>,
}

impl std::fmt::Debug for FilterState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterState")
            .field("omitted", &self.omitted.len())
            .finish_non_exhaustive()
    }
}

impl FilterState {
    /// The state for `filter`. `collect` keeps the omitted objects (and
    /// makes the walk enter omitted trees to find them). `sparse` reads
    /// the patterns of a `sparse:oid=<blob-ish>` filter.
    pub fn new<E>(
        filter: &ObjectFilter,
        collect: bool,
        sparse: &mut SparseLoader<'_, E>,
    ) -> Result<FilterState, E> {
        let node = match filter {
            ObjectFilter::BlobNone => Node::BlobLimit(0),
            ObjectFilter::BlobLimit(n) => Node::BlobLimit(*n),
            ObjectFilter::TreeDepth(n) => Node::Tree {
                limit: *n,
                depths: HashMap::new(),
            },
            ObjectFilter::ObjectType(k) => Node::Type(*k),
            ObjectFilter::SparseOid(spec) => Node::Sparse {
                patterns: sparse(spec)?,
                stack: Vec::new(),
                walked: HashSet::new(),
            },
            ObjectFilter::Combine(subs) => {
                let mut states = Vec::new();
                for s in subs {
                    states.push(FilterState::new(s, collect, sparse)?);
                }
                Node::Combine {
                    skipping: vec![0; states.len()],
                    subs: states,
                }
            }
        };
        Ok(FilterState {
            node,
            collect,
            omitted: HashSet::new(),
        })
    }

    /// Whether a commit or tag reached by the walk is shown.
    pub fn shows(&self, kind: ObjectKind) -> bool {
        match &self.node {
            Node::Type(k) => *k == kind,
            Node::Combine { subs, .. } => subs.iter().all(|s| s.shows(kind)),
            _ => true,
        }
    }

    /// Whether trees need to be walked at all (`object:type=commit` and
    /// `=tag` need no trees).
    pub fn walks_trees(&self) -> bool {
        match &self.node {
            Node::Type(k) => matches!(k, ObjectKind::Tree | ObjectKind::Blob),
            Node::Combine { subs, .. } => subs.iter().all(FilterState::walks_trees),
            _ => true,
        }
    }

    /// A tree at `path` (empty for a root tree), `depth` levels below the
    /// root, is reached.
    pub fn begin_tree(&mut self, id: &ObjectId, path: &[u8], depth: u64) -> Verdict {
        let collect = self.collect;
        match &mut self.node {
            Node::BlobLimit(_) => Verdict::SHOW,
            Node::Type(k) => Verdict {
                show: *k == ObjectKind::Tree,
                mark_seen: true,
                skip: !matches!(k, ObjectKind::Tree | ObjectKind::Blob),
            },
            Node::Tree { limit, depths } => {
                let before = depths.get(id).copied();
                if before.is_some_and(|d| d <= depth) {
                    // Walked already at this depth or above.
                    return Verdict {
                        show: false,
                        mark_seen: false,
                        skip: true,
                    };
                }
                depths.insert(*id, depth);
                if depth >= *limit {
                    self.omitted.insert(*id);
                    Verdict {
                        show: false,
                        mark_seen: false,
                        skip: !collect || before.is_some(),
                    }
                } else {
                    Verdict {
                        show: true,
                        mark_seen: false,
                        skip: false,
                    }
                }
            }
            Node::Sparse {
                patterns,
                stack,
                walked,
            } => {
                let parent = stack.last().copied().unwrap_or(false);
                let decision = if path.is_empty() {
                    parent
                } else {
                    patterns.decide(path, true).unwrap_or(parent)
                };
                if !walked.insert((*id, decision)) {
                    return Verdict {
                        show: false,
                        mark_seen: false,
                        skip: true,
                    };
                }
                stack.push(decision);
                Verdict {
                    show: true,
                    mark_seen: false,
                    skip: false,
                }
            }
            Node::Combine { subs, skipping } => {
                let mut all = Verdict {
                    show: true,
                    mark_seen: true,
                    skip: true,
                };
                for (s, skip) in subs.iter_mut().zip(skipping.iter_mut()) {
                    let v = if *skip > 0 {
                        *skip += 1;
                        HIDDEN
                    } else {
                        let v = s.begin_tree(id, path, depth);
                        if v.skip {
                            *skip = 1;
                        }
                        v
                    };
                    all.show &= v.show;
                    all.mark_seen &= v.mark_seen;
                    all.skip &= v.skip;
                }
                if all.skip {
                    // The walker does not enter the tree.
                    for skip in skipping.iter_mut() {
                        *skip -= 1;
                    }
                }
                all
            }
        }
    }

    /// The walk is done with the entries of a tree [`begin_tree`]
    /// let it enter.
    ///
    /// [`begin_tree`]: FilterState::begin_tree
    pub fn end_tree(&mut self) {
        match &mut self.node {
            Node::Sparse { stack, .. } => {
                stack.pop();
            }
            Node::Combine { subs, skipping } => {
                for (s, skip) in subs.iter_mut().zip(skipping.iter_mut()) {
                    if *skip > 0 {
                        *skip -= 1;
                    } else {
                        s.end_tree();
                    }
                }
            }
            _ => {}
        }
    }

    /// A blob at `path`, `depth` levels below the root, of `size` bytes
    /// (when known) is reached.
    pub fn blob(&mut self, id: &ObjectId, path: &[u8], depth: u64, size: Option<u64>) -> Verdict {
        match &mut self.node {
            Node::BlobLimit(limit) => {
                if size.is_some_and(|s| s >= *limit) || *limit == 0 {
                    self.omitted.insert(*id);
                    Verdict {
                        show: false,
                        mark_seen: true,
                        skip: false,
                    }
                } else {
                    Verdict::SHOW
                }
            }
            Node::Type(k) => Verdict {
                show: *k == ObjectKind::Blob,
                mark_seen: true,
                skip: false,
            },
            Node::Tree { limit, depths } => {
                let before = depths.get(id).copied();
                if before.is_some_and(|d| d <= depth) {
                    return Verdict {
                        show: false,
                        mark_seen: false,
                        skip: false,
                    };
                }
                depths.insert(*id, depth);
                if depth >= *limit {
                    self.omitted.insert(*id);
                    Verdict {
                        show: false,
                        mark_seen: false,
                        skip: false,
                    }
                } else {
                    Verdict::SHOW
                }
            }
            Node::Sparse {
                patterns, stack, ..
            } => {
                let parent = stack.last().copied().unwrap_or(false);
                if patterns.decide(path, false).unwrap_or(parent) {
                    Verdict::SHOW
                } else {
                    self.omitted.insert(*id);
                    Verdict {
                        show: false,
                        mark_seen: false,
                        skip: false,
                    }
                }
            }
            Node::Combine { subs, skipping } => {
                let mut all = Verdict::SHOW;
                for (s, skip) in subs.iter_mut().zip(skipping.iter()) {
                    let v = if *skip > 0 {
                        HIDDEN
                    } else {
                        s.blob(id, path, depth, size)
                    };
                    all.show &= v.show;
                    all.mark_seen &= v.mark_seen;
                }
                all.skip = false;
                all
            }
        }
    }

    /// The objects some filter omitted, including those shown afterwards
    /// (the caller removes those).
    pub fn omitted(&self) -> HashSet<ObjectId> {
        let mut out = self.omitted.clone();
        if let Node::Combine { subs, .. } = &self.node {
            for s in subs {
                out.extend(s.omitted());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
    }

    fn state(spec: &str, collect: bool) -> FilterState {
        let f = ObjectFilter::parse(spec).unwrap();
        FilterState::new(&f, collect, &mut |_| -> Result<
            Box<dyn SparsePatterns>,
            (),
        > {
            struct Dir1;
            impl SparsePatterns for Dir1 {
                fn decide(&self, path: &[u8], is_dir: bool) -> Option<bool> {
                    (is_dir && path == b"dir1").then_some(true)
                }
            }
            Ok(Box::new(Dir1))
        })
        .unwrap()
    }

    #[test]
    fn blob_limit() {
        let mut s = state("blob:limit=10", true);
        assert!(s.begin_tree(&id(1), b"", 0).show);
        assert!(s.blob(&id(2), b"a", 1, Some(9)).show);
        assert!(!s.blob(&id(3), b"b", 1, Some(10)).show);
        assert_eq!(s.omitted(), HashSet::from([id(3)]));
    }

    #[test]
    fn tree_depth_counts_the_smallest_depth() {
        let mut s = state("tree:2", false);
        assert!(s.begin_tree(&id(1), b"", 0).show);
        let deep = s.begin_tree(&id(2), b"a", 1);
        assert!(deep.show && !deep.skip);
        assert!(!s.blob(&id(3), b"a/x", 2, None).show);
        s.end_tree();
        // The same blob higher up is shown after all.
        assert!(s.blob(&id(3), b"x", 1, None).show);
        // A tree omitted and not collected is skipped.
        let v = s.begin_tree(&id(4), b"a/b", 2);
        assert!(!v.show && v.skip);
    }

    #[test]
    fn combine_skips_only_when_all_skip() {
        let mut s = state("combine:tree:1+tree:3", false);
        assert!(s.begin_tree(&id(1), b"", 0).show);
        let v = s.begin_tree(&id(2), b"d", 1);
        assert!(!v.show && !v.skip);
        s.end_tree();
        let v = s.begin_tree(&id(2), b"d", 1);
        assert!(v.skip);
    }

    #[test]
    fn sparse_inherits_directory_decisions() {
        let mut s = state("sparse:oid=x", true);
        assert!(s.begin_tree(&id(1), b"", 0).show);
        assert!(!s.blob(&id(2), b"top", 1, None).show);
        assert!(s.begin_tree(&id(3), b"dir1", 1).show);
        assert!(s.blob(&id(4), b"dir1/f", 2, None).show);
        s.end_tree();
        s.end_tree();
        assert_eq!(s.omitted(), HashSet::from([id(2)]));
    }

    #[test]
    fn object_type() {
        let s = state("object:type=blob", false);
        assert!(!s.shows(ObjectKind::Commit));
        let mut s = state("object:type=commit", false);
        assert!(s.shows(ObjectKind::Commit));
        assert!(s.begin_tree(&id(1), b"", 0).skip);
    }
}
