// SPDX-License-Identifier: MIT
//! A notes tree in memory.
//!
//! The notes of a tree are kept in a trie over the hex digits of the
//! annotated object ids (16 children per node). Written trees get their
//! fan-out from that trie, as observed with the `git` binary:
//!
//! - Reading a note found below `k` fan-out directories (a prefix of `2k`
//!   digits) places it no higher than the node at depth `2k - 1`, so the
//!   trie keeps the shape of the tree it was read from.
//! - A new note takes the first free slot on its path; two notes in one
//!   slot move one level down. Removing a note drops nodes that become
//!   empty, nothing else.
//! - Writing starts at fan-out 0. At every node at an even depth `n` with
//!   `n <= 2 * fanout`, the fan-out grows by one when all 16 children of
//!   the node are nodes themselves.
//!
//! So the fan-out grows when a level fills up and shrinks only when notes
//! are removed until a level is no longer full, which keeps the trees of
//! successive notes commits stable.
//!
//! Entries that are not notes (names that are neither a full object id nor
//! a two-digit fan-out directory) are kept at their paths.

use std::collections::BTreeMap;

use rgit_hash::{HashKind, ObjectId};
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry, TreeIter};
use rgit_odb::ObjectDatabase;

use crate::{Combine, Error};

/// An entry of a notes tree that is not a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonNote {
    /// The path below the root of the notes tree.
    pub path: Vec<u8>,
    /// The mode of the entry.
    pub mode: EntryMode,
    /// The blob or tree.
    pub oid: ObjectId,
}

#[derive(Clone, Copy, Debug)]
struct Leaf {
    key: ObjectId,
    note: ObjectId,
}

#[derive(Clone, Debug, Default)]
enum Slot {
    #[default]
    Empty,
    Leaf(Leaf),
    Node(Box<Node>),
}

#[derive(Clone, Debug, Default)]
struct Node {
    slots: [Slot; 16],
}

impl Node {
    fn is_empty(&self) -> bool {
        self.slots.iter().all(|s| matches!(s, Slot::Empty))
    }
}

/// The `i`-th hex digit of `id`.
fn nibble(id: &ObjectId, i: usize) -> usize {
    let b = id.as_bytes()[i / 2];
    usize::from(if i % 2 == 0 { b >> 4 } else { b & 0x0f })
}

/// The notes of one notes tree.
#[derive(Clone, Debug)]
pub struct Notes {
    hash: HashKind,
    root: Node,
    non_notes: Vec<NonNote>,
}

impl Notes {
    /// No notes.
    pub fn new(hash: HashKind) -> Notes {
        Notes {
            hash,
            root: Node::default(),
            non_notes: Vec::new(),
        }
    }

    /// The notes of `tree` (`None`: no notes). Notes found at more than one
    /// path for the same object are concatenated (the one in the deeper
    /// directory first), as `git` shows them.
    pub fn load(odb: &ObjectDatabase, tree: Option<&ObjectId>) -> Result<Notes, Error> {
        let mut notes = Notes::new(odb.hash_kind());
        if let Some(tree) = tree {
            let mut prefix = Vec::new();
            notes.load_tree(odb, tree, &mut prefix, b"")?;
        }
        Ok(notes)
    }

    /// Reads tree `id`, whose entries continue the hex digits `prefix`
    /// (path `path` below the root).
    fn load_tree(
        &mut self,
        odb: &ObjectDatabase,
        id: &ObjectId,
        prefix: &mut Vec<u8>,
        path: &[u8],
    ) -> Result<(), Error> {
        let data = match odb.read(id)? {
            Some(raw) if raw.kind == ObjectKind::Tree => raw.data,
            _ => return Err(Error::BadTree(*id)),
        };
        let hex_len = self.hash.hex_len();
        for entry in TreeIter::new(self.hash, &data) {
            let entry = entry.map_err(|_| Error::BadTree(*id))?;
            let name = entry.name;
            let is_tree = entry.mode_bits & 0o170000 == 0o040000;
            let all_hex = name.iter().all(u8::is_ascii_hexdigit);
            if all_hex && !is_tree && prefix.len() + name.len() == hex_len {
                let mut hex = prefix.clone();
                hex.extend_from_slice(name);
                if let Ok(key) = ObjectId::from_hex(self.hash, &hex.to_ascii_lowercase()) {
                    let force = prefix.len().saturating_sub(1);
                    self.insert(key, entry.oid, force, &mut |cur, new| {
                        crate::concatenate(odb, cur, new)
                    })?;
                    continue;
                }
            }
            if all_hex && is_tree && name.len() == 2 && prefix.len() + 2 < hex_len {
                let len = prefix.len();
                prefix.extend_from_slice(&name.to_ascii_lowercase());
                let mut sub = path.to_vec();
                sub.extend_from_slice(name);
                sub.push(b'/');
                self.load_tree(odb, &entry.oid, prefix, &sub)?;
                prefix.truncate(len);
                continue;
            }
            let mut full = path.to_vec();
            full.extend_from_slice(name);
            self.non_notes.push(NonNote {
                path: full,
                mode: EntryMode::from_octal(entry.mode_digits)
                    .unwrap_or(EntryMode::from_bits(entry.mode_bits)),
                oid: entry.oid,
            });
        }
        Ok(())
    }

    /// The hash algorithm of the annotated objects.
    pub fn hash(&self) -> HashKind {
        self.hash
    }

    /// The note of `key`.
    pub fn get(&self, key: &ObjectId) -> Option<ObjectId> {
        let mut node = &self.root;
        let mut depth = 0;
        loop {
            match &node.slots[nibble(key, depth)] {
                Slot::Empty => return None,
                Slot::Leaf(l) => return (l.key == *key).then_some(l.note),
                Slot::Node(n) => {
                    node = n;
                    depth += 1;
                }
            }
        }
    }

    /// Attaches `note` to `key`; an existing note is combined with it.
    pub fn add(
        &mut self,
        odb: &ObjectDatabase,
        key: ObjectId,
        note: ObjectId,
        combine: Combine,
    ) -> Result<(), Error> {
        self.insert(key, note, 0, &mut |cur, new| combine.apply(odb, cur, new))
    }

    /// Attaches `note` to `key`, replacing an existing note.
    pub fn set(&mut self, key: ObjectId, note: ObjectId) {
        let _ = self.insert(key, note, 0, &mut |_, new| Ok(new));
    }

    fn insert(
        &mut self,
        key: ObjectId,
        note: ObjectId,
        force: usize,
        combine: &mut dyn FnMut(ObjectId, ObjectId) -> Result<ObjectId, Error>,
    ) -> Result<(), Error> {
        insert_at(&mut self.root, 0, Leaf { key, note }, force, combine)
    }

    /// Removes the note of `key`; returns it.
    pub fn remove(&mut self, key: &ObjectId) -> Option<ObjectId> {
        remove_at(&mut self.root, 0, key)
    }

    /// All notes as `(annotated object, note)`, sorted by object.
    pub fn list(&self) -> Vec<(ObjectId, ObjectId)> {
        let mut out = Vec::new();
        collect(&self.root, &mut |l, _| out.push((l.key, l.note)), 0, 0);
        out
    }

    /// Whether there are no notes (non-notes do not count).
    pub fn is_empty(&self) -> bool {
        self.root.is_empty()
    }

    /// The entries that are not notes.
    pub fn non_notes(&self) -> &[NonNote] {
        &self.non_notes
    }

    /// Writes the notes tree (with fan-out directories and the non-notes)
    /// and returns its id.
    pub fn write(&self, odb: &ObjectDatabase) -> Result<ObjectId, Error> {
        let mut root = Dir::default();
        let mut paths = Vec::new();
        collect(
            &self.root,
            &mut |l, fanout| {
                let hex = l.key.to_hex().into_bytes();
                let mut path = Vec::with_capacity(hex.len() + fanout);
                for i in 0..fanout {
                    path.extend_from_slice(&hex[2 * i..2 * i + 2]);
                    path.push(b'/');
                }
                path.extend_from_slice(&hex[2 * fanout..]);
                paths.push((path, l.note));
            },
            0,
            0,
        );
        for (path, note) in paths {
            root.insert(&path, EntryMode::BLOB, note);
        }
        for n in &self.non_notes {
            root.insert(&n.path, n.mode.clone(), n.oid);
        }
        root.write(odb)
    }
}

fn insert_at(
    node: &mut Node,
    depth: usize,
    leaf: Leaf,
    force: usize,
    combine: &mut dyn FnMut(ObjectId, ObjectId) -> Result<ObjectId, Error>,
) -> Result<(), Error> {
    let i = nibble(&leaf.key, depth);
    let slot = &mut node.slots[i];
    match slot {
        Slot::Empty => {
            if depth >= force {
                *slot = Slot::Leaf(leaf);
                return Ok(());
            }
            *slot = Slot::Node(Box::default());
        }
        Slot::Leaf(old) => {
            if depth >= force && old.key == leaf.key {
                old.note = combine(old.note, leaf.note)?;
                return Ok(());
            }
            let old = *old;
            let mut n = Box::<Node>::default();
            n.slots[nibble(&old.key, depth + 1)] = Slot::Leaf(old);
            *slot = Slot::Node(n);
        }
        Slot::Node(_) => {}
    }
    match slot {
        Slot::Node(n) => insert_at(n, depth + 1, leaf, force, combine),
        _ => unreachable!("slot was made a node"),
    }
}

fn remove_at(node: &mut Node, depth: usize, key: &ObjectId) -> Option<ObjectId> {
    let slot = &mut node.slots[nibble(key, depth)];
    match slot {
        Slot::Empty => None,
        Slot::Leaf(l) => {
            if l.key != *key {
                return None;
            }
            let note = l.note;
            *slot = Slot::Empty;
            Some(note)
        }
        Slot::Node(n) => {
            let found = remove_at(n, depth + 1, key);
            if n.is_empty() {
                *slot = Slot::Empty;
            }
            found
        }
    }
}

/// Calls `f` with every leaf and its fan-out, in order.
fn collect(node: &Node, f: &mut dyn FnMut(&Leaf, usize), depth: usize, mut fanout: usize) {
    if depth % 2 == 0
        && depth <= 2 * fanout
        && node.slots.iter().all(|s| matches!(s, Slot::Node(_)))
    {
        fanout += 1;
    }
    for s in &node.slots {
        match s {
            Slot::Empty => {}
            Slot::Leaf(l) => f(l, fanout),
            Slot::Node(n) => collect(n, f, depth + 1, fanout),
        }
    }
}

/// A directory of the tree being written.
#[derive(Default)]
struct Dir {
    entries: BTreeMap<Vec<u8>, Item>,
}

enum Item {
    File(EntryMode, ObjectId),
    Dir(Dir),
}

impl Dir {
    fn insert(&mut self, path: &[u8], mode: EntryMode, oid: ObjectId) {
        match path.iter().position(|&c| c == b'/') {
            None => {
                self.entries
                    .entry(path.to_vec())
                    .or_insert(Item::File(mode, oid));
            }
            Some(p) => {
                let item = self
                    .entries
                    .entry(path[..p].to_vec())
                    .or_insert_with(|| Item::Dir(Dir::default()));
                if let Item::File(..) = item {
                    *item = Item::Dir(Dir::default());
                }
                if let Item::Dir(d) = item {
                    d.insert(&path[p + 1..], mode, oid);
                }
            }
        }
    }

    fn write(&self, odb: &ObjectDatabase) -> Result<ObjectId, Error> {
        let mut tree = Tree::new();
        for (name, item) in &self.entries {
            let entry = match item {
                Item::File(mode, oid) => TreeEntry::new(mode.clone(), name.clone(), *oid),
                Item::Dir(d) => TreeEntry::new(EntryMode::TREE, name.clone(), d.write(odb)?),
            };
            tree.entries.push(entry);
        }
        tree.sort();
        Ok(odb.write(ObjectKind::Tree, &tree.serialize())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(hex: &str) -> ObjectId {
        let mut s = hex.to_owned();
        while s.len() < 40 {
            s.push('0');
        }
        ObjectId::from_hex(HashKind::Sha1, s.as_bytes()).unwrap()
    }

    fn fanouts(n: &Notes) -> Vec<usize> {
        let mut out = Vec::new();
        collect(&n.root, &mut |_, f| out.push(f), 0, 0);
        out
    }

    #[test]
    fn full_level_of_nodes_fans_out() {
        let mut n = Notes::new(HashKind::Sha1);
        let note = id("ff");
        // Two notes under every first digit: all 16 root slots are nodes.
        for d in "0123456789abcdef".chars() {
            n.set(id(&format!("{d}0")), note);
            if d != 'f' {
                n.set(id(&format!("{d}1")), note);
            }
        }
        assert!(fanouts(&n).iter().all(|&f| f == 0));
        n.set(id("f1"), note);
        assert!(fanouts(&n).iter().all(|&f| f == 1));
        // Removing keeps the node of "f" (it still holds a leaf).
        n.remove(&id("f1"));
        assert!(fanouts(&n).iter().all(|&f| f == 1));
        n.remove(&id("f0"));
        assert!(fanouts(&n).iter().all(|&f| f == 0));
    }

    #[test]
    fn get_and_list_are_sorted() {
        let mut n = Notes::new(HashKind::Sha1);
        n.set(id("b2"), id("01"));
        n.set(id("a1"), id("02"));
        n.set(id("b3"), id("03"));
        assert_eq!(n.get(&id("b3")), Some(id("03")));
        assert_eq!(n.get(&id("b4")), None);
        let keys: Vec<_> = n.list().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec![id("a1"), id("b2"), id("b3")]);
        assert_eq!(n.remove(&id("b2")), Some(id("01")));
        assert_eq!(n.list().len(), 2);
    }
}
