// SPDX-License-Identifier: MIT

//! Rebuilding the cache tree of an unpacked index.
//!
//! After a merge git records a cache tree node for every directory of the
//! index and marks it valid only when the tree object it would produce
//! already exists in the repository (so a directory taken over unchanged
//! from a tree is valid, a directory with carried-over local changes is
//! not). Observed with `git read-tree -m` and checked by the differential
//! tests.

use rgit_hash::{HashKind, ObjectId, hash_object};
use rgit_index::{CacheTree, Entry, Index, TreeNode};

use crate::objects::Objects;

/// The cache tree for `index`: one node per directory, valid where the
/// tree object exists in `odb`.
pub fn repair_cache_tree(index: &Index, odb: &dyn Objects) -> CacheTree {
    let entries = index.entries();
    let (root, _) = build(entries, 0, b"", odb.hash_kind(), odb);
    CacheTree { root }
}

/// Builds the node for the entries (all below the same directory, whose
/// path prefix is `strip` bytes long) and returns its tree id when it can
/// be computed.
fn build(
    entries: &[Entry],
    strip: usize,
    name: &[u8],
    kind: HashKind,
    odb: &dyn Objects,
) -> (TreeNode, Option<ObjectId>) {
    let mut children = Vec::new();
    // (name, is_tree, mode digits, id)
    let mut items: Vec<(Vec<u8>, bool, String, ObjectId)> = Vec::new();
    let mut computable = true;
    let mut i = 0;
    while i < entries.len() {
        let rest = &entries[i].path()[strip..];
        match rest.iter().position(|&b| b == b'/') {
            None => {
                let e = &entries[i];
                if e.stage().is_conflict() || e.flags.intent_to_add {
                    computable = false;
                } else {
                    items.push((rest.to_vec(), false, format!("{:o}", e.mode.0), e.id));
                }
                i += 1;
            }
            Some(slash) => {
                let dir = &rest[..=slash];
                let mut j = i + 1;
                while j < entries.len() && entries[j].path()[strip..].starts_with(dir) {
                    j += 1;
                }
                let (node, id) =
                    build(&entries[i..j], strip + slash + 1, &rest[..slash], kind, odb);
                children.push(node);
                match id {
                    Some(id) => items.push((rest[..slash].to_vec(), true, "40000".into(), id)),
                    None => computable = false,
                }
                i = j;
            }
        }
    }
    children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    let id = if computable {
        items.sort_by(|a, b| rgit_object::entry_cmp(&a.0, a.1, &b.0, b.1));
        let mut data = Vec::new();
        for (n, _, mode, id) in &items {
            data.extend_from_slice(mode.as_bytes());
            data.push(b' ');
            data.extend_from_slice(n);
            data.push(0);
            data.extend_from_slice(id.as_bytes());
        }
        hash_object(kind, "tree", &data).ok()
    } else {
        None
    };
    let mut node = match id {
        Some(id) if odb.has(&id) => TreeNode::new_valid(
            name.to_vec(),
            u32::try_from(entries.len()).unwrap_or(u32::MAX),
            id,
        ),
        _ => TreeNode::new_invalid(name.to_vec()),
    };
    node.children = children;
    (node, id)
}
