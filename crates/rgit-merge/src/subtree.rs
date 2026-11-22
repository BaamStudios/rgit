// SPDX-License-Identifier: MIT

//! Shifting trees for subtree merges (`-s subtree`, `-X subtree=<path>`,
//! Documentation/merge-strategies.adoc): when one side of a merge is a
//! subdirectory of the other, the other trees are moved down into (or up
//! out of) that subdirectory so that all three have our tree's shape.
//!
//! Without an explicit path the shift is guessed: every directory of our
//! tree is compared with the top of the other tree (and the other way
//! round), and the pair with the most entries in common wins if it shares
//! more than the two top-level trees do. The scoring is rgit's own.

use rgit_hash::ObjectId;
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};

use crate::engine::read_tree;
use crate::{Error, Objects, Version};

const MODE_TREE: u32 = 0o040000;
/// How deep the guess looks for matching directories.
const MAX_DEPTH: usize = 8;

/// How a subtree merge shifts the other trees.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Subtree {
    /// No shift (a normal merge).
    #[default]
    None,
    /// `-s subtree`: guess the shift.
    Guess,
    /// `-X subtree=<path>`: shift by this path.
    Path(Vec<u8>),
}

/// The shift that turns `other` into our shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shift {
    None,
    /// Put `other` below this directory.
    Prefix(Vec<u8>),
    /// Take this directory of `other`.
    Strip(Vec<u8>),
}

/// The shift that turns `theirs` into our shape, as `how` asks.
pub(crate) fn find(
    objects: &dyn Objects,
    how: &Subtree,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> Result<Shift, Error> {
    if ours == theirs {
        return Ok(Shift::None);
    }
    Ok(match how {
        Subtree::None => Shift::None,
        Subtree::Guess => guess(objects, ours, theirs)?,
        Subtree::Path(p) => {
            let p: Vec<u8> = trim_slashes(p).to_vec();
            if p.is_empty() {
                Shift::None
            } else {
                let in_ours = subtree_at(objects, ours, &p)?.is_some();
                let in_theirs = subtree_at(objects, theirs, &p)?.is_some();
                match (in_ours, in_theirs) {
                    (false, true) => Shift::Strip(p),
                    (true, false) => Shift::Prefix(p),
                    _ => Shift::None,
                }
            }
        }
    })
}

/// `tree` shifted by `shift` into the shape of `ours`: a prefix puts it
/// in place of that directory of `ours` (the rest of `ours` stays, as with
/// git, which writes the new trees).
pub(crate) fn apply(
    objects: &dyn Objects,
    shift: &Shift,
    ours: &ObjectId,
    tree: &ObjectId,
) -> Result<ObjectId, Error> {
    match shift {
        Shift::None => Ok(*tree),
        Shift::Strip(p) => {
            Ok(subtree_at(objects, tree, p)?.unwrap_or_else(|| objects.hash_kind().empty_tree()))
        }
        Shift::Prefix(p) => {
            let names: Vec<&[u8]> = p.split(|&b| b == b'/').filter(|n| !n.is_empty()).collect();
            splice(objects, Some(*ours), &names, *tree)
        }
    }
}

/// `at` (a tree, or nothing) with the tree at `path` replaced by `tree`.
fn splice(
    objects: &dyn Objects,
    at: Option<ObjectId>,
    path: &[&[u8]],
    tree: ObjectId,
) -> Result<ObjectId, Error> {
    let Some((name, rest)) = path.split_first() else {
        return Ok(tree);
    };
    let mut entries = match at {
        Some(id) => read_tree(objects, &id)?,
        None => Vec::new(),
    };
    let pos = entries.iter().position(|(n, _)| n == name);
    let below = pos.and_then(|i| (entries[i].1.mode == MODE_TREE).then_some(entries[i].1.oid));
    let new = splice(objects, below, rest, tree)?;
    let v = Version {
        mode: MODE_TREE,
        oid: new,
    };
    match pos {
        Some(i) => entries[i].1 = v,
        None => entries.push((name.to_vec(), v)),
    }
    let mut t = Tree {
        entries: entries
            .into_iter()
            .map(|(n, v)| TreeEntry::new(EntryMode::from_bits(v.mode), n, v.oid))
            .collect(),
    };
    t.sort();
    objects
        .write(ObjectKind::Tree, &t.serialize())
        .map_err(Error::Write)
}

/// Shifts `base` and `theirs` to the shape of `ours`, each on its own (as
/// git does: a base that matches another directory of ours better is
/// shifted there).
pub(crate) fn shift_both(
    objects: &dyn Objects,
    how: &Subtree,
    ours: &ObjectId,
    base: &ObjectId,
    theirs: &ObjectId,
) -> Result<(ObjectId, ObjectId), Error> {
    let base_shift = find(objects, how, ours, base)?;
    let their_shift = find(objects, how, ours, theirs)?;
    Ok((
        apply(objects, &base_shift, ours, base)?,
        apply(objects, &their_shift, ours, theirs)?,
    ))
}

fn trim_slashes(p: &[u8]) -> &[u8] {
    let start = p.iter().position(|&b| b != b'/').unwrap_or(p.len());
    let end = p.iter().rposition(|&b| b != b'/').map_or(start, |e| e + 1);
    &p[start..end.max(start)]
}

/// The tree at `path` below `tree`, if there is one.
fn subtree_at(
    objects: &dyn Objects,
    tree: &ObjectId,
    path: &[u8],
) -> Result<Option<ObjectId>, Error> {
    let mut cur = *tree;
    for name in path.split(|&b| b == b'/').filter(|n| !n.is_empty()) {
        let entries = read_tree(objects, &cur)?;
        match entries
            .iter()
            .find(|(n, v)| n == name && v.mode == MODE_TREE)
        {
            Some((_, v)) => cur = v.oid,
            None => return Ok(None),
        }
    }
    Ok(Some(cur))
}

/// How well two directory listings match: entries with the same name and
/// object count twice, the same name once, names on one side only count
/// against the match.
fn score(a: &[(Vec<u8>, Version)], b: &[(Vec<u8>, Version)]) -> i64 {
    let mut s = 0i64;
    for (name, v) in b {
        match a.iter().find(|(n, _)| n == name) {
            Some((_, w)) if w.oid == v.oid => s += 2,
            Some(_) => s += 1,
            None => s -= 1,
        }
    }
    s - a
        .iter()
        .filter(|(n, _)| !b.iter().any(|(m, _)| m == n))
        .count() as i64
}

/// Every directory below `tree` (not the tree itself) with its path, up
/// to [`MAX_DEPTH`] levels deep.
fn directories(
    objects: &dyn Objects,
    tree: &ObjectId,
    prefix: &mut Vec<u8>,
    depth: usize,
    out: &mut Vec<(Vec<u8>, ObjectId)>,
) -> Result<(), Error> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    for (name, v) in read_tree(objects, tree)? {
        if v.mode != MODE_TREE {
            continue;
        }
        let len = prefix.len();
        if !prefix.is_empty() {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(&name);
        out.push((prefix.clone(), v.oid));
        directories(objects, &v.oid, prefix, depth + 1, out)?;
        prefix.truncate(len);
    }
    Ok(())
}

fn guess(objects: &dyn Objects, ours: &ObjectId, other: &ObjectId) -> Result<Shift, Error> {
    let ours_top = read_tree(objects, ours)?;
    let other_top = read_tree(objects, other)?;
    let mut best = (score(&ours_top, &other_top), Shift::None);
    let mut dirs = Vec::new();
    directories(objects, ours, &mut Vec::new(), 0, &mut dirs)?;
    for (path, id) in dirs {
        let s = if id == *other {
            i64::MAX
        } else {
            score(&read_tree(objects, &id)?, &other_top)
        };
        if s > best.0 {
            best = (s, Shift::Prefix(path));
        }
    }
    let mut dirs = Vec::new();
    directories(objects, other, &mut Vec::new(), 0, &mut dirs)?;
    for (path, id) in dirs {
        let s = if id == *ours {
            i64::MAX
        } else {
            score(&ours_top, &read_tree(objects, &id)?)
        };
        if s > best.0 {
            best = (s, Shift::Strip(path));
        }
    }
    Ok(best.1)
}
