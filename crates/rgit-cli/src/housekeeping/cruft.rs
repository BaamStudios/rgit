// SPDX-License-Identifier: MIT
//! The contents of a cruft pack (`git pack-objects --cruft`,
//! Documentation/git-pack-objects.adoc and gitformat-pack(5), "cruft
//! packs").
//!
//! The input names packs: without prefix the ones that stay, with `-` the
//! ones about to be deleted. Packs that are not named stay as well. The
//! candidates are the loose objects and the objects of the packs about to
//! be deleted, except those that a staying pack has. Each candidate's
//! time is the newest of its copies: the loose file's modification time,
//! the `.pack` file's for an ordinary pack, the recorded one for a cruft
//! pack.
//!
//! Without an expiration every candidate goes into the cruft pack. With
//! one, the candidates newer than it go in, the objects named by
//! `gc.recentObjectsHook`, and every candidate reachable from those
//! (traversal stops at objects of staying packs; missing objects are
//! skipped). Such rescued old objects are recorded with the expiration
//! time itself, as git does.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufRead;

use rgit_hash::ObjectId;
use rgit_object::{Commit, ObjectKind, Tag, TreeIter};
use rgit_odb::ObjectDatabase;
use rgit_pack::write::{self as pw, ListedObject};

use super::store::{self, LocalPack};
use crate::Die;
use crate::messages::housekeeping as msg;

/// The objects of a cruft pack in pack order, with their times.
pub struct Selection {
    pub objects: Vec<ListedObject>,
    pub mtimes: HashMap<ObjectId, u32>,
}

/// Read the pack list of `pack-objects --cruft`: returns per pack of
/// `packs` whether it is about to be deleted. An unknown name is fatal.
pub fn read_pack_list(input: &mut dyn BufRead, packs: &[LocalPack]) -> Result<Vec<bool>, Die> {
    let mut discarded = vec![false; packs.len()];
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
        let name = line.trim_end_matches(['\n', '\r']);
        if name.is_empty() {
            continue;
        }
        let (gone, name) = match name.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, name),
        };
        let Some(i) = packs.iter().position(|p| p.file_name == name) else {
            return Err(Die::Fatal(msg::could_not_find_pack(name)));
        };
        if gone {
            discarded[i] = true;
        }
    }
    Ok(discarded)
}

/// Seconds as a `.mtimes` entry.
fn entry_time(t: u64) -> u32 {
    t.min(u64::from(u32::MAX)) as u32
}

/// Choose the cruft objects. `discarded[i]` marks `packs[i]` as about to
/// be deleted; `expire` is the cut-off (seconds), `recent` the objects
/// named by `gc.recentObjectsHook`; with `local` objects an alternate
/// has are left out.
pub fn select(
    odb: &ObjectDatabase,
    packs: &[LocalPack],
    discarded: &[bool],
    expire: Option<u64>,
    recent: &[ObjectId],
    local: bool,
) -> Result<Selection, Die> {
    let kind = odb.hash_kind();
    let staying: Vec<&LocalPack> = packs
        .iter()
        .zip(discarded)
        .filter(|(_, d)| !**d)
        .map(|(p, _)| p)
        .collect();
    let stays = |id: &ObjectId| staying.iter().any(|p| p.pack.contains(id));
    let in_alternate = |id: &ObjectId| odb.stores().iter().skip(2).any(|s| s.contains(id));

    // Candidates in a stable order: the packs about to go in pack order,
    // then the loose objects by id.
    let mut order: Vec<ObjectId> = Vec::new();
    let mut times: HashMap<ObjectId, u64> = HashMap::new();
    let mut offer = |id: ObjectId, t: u64, order: &mut Vec<ObjectId>| {
        if stays(&id) || (local && in_alternate(&id)) {
            return;
        }
        match times.get_mut(&id) {
            Some(old) => *old = (*old).max(t),
            None => {
                times.insert(id, t);
                order.push(id);
            }
        }
    };
    for (p, _) in packs.iter().zip(discarded).filter(|(_, d)| **d) {
        let index = p.pack.index();
        let mut by_offset: Vec<(u64, u32)> = (0..index.len())
            .filter_map(|pos| index.offset_at(pos).ok().map(|off| (off, pos)))
            .collect();
        by_offset.sort_unstable();
        let cruft = p.pack.is_cruft();
        for (_, pos) in by_offset {
            let id = index.id_at(pos);
            let t = if cruft {
                p.pack
                    .mtimes()
                    .and_then(|m| m.get(pos))
                    .map_or(p.mtime, u64::from)
            } else {
                p.mtime
            };
            offer(id, t, &mut order);
        }
    }
    let objects_dir = odb.loose().objects_dir().to_path_buf();
    let mut loose = store::loose_objects(&objects_dir, kind);
    loose.sort_by_key(|a| a.0);
    for (id, _, t) in loose {
        offer(id, t, &mut order);
    }

    let chosen: HashMap<ObjectId, u32> = match expire {
        None => times.iter().map(|(id, t)| (*id, entry_time(*t))).collect(),
        Some(cut) => {
            // What rescued objects record: the cut-off, at most the
            // present ("now" expires everything).
            let now = super::secs(Ok(std::time::SystemTime::now()));
            let floor = cut.min(now);
            let mut chosen: HashMap<ObjectId, u32> = HashMap::new();
            let mut queue: VecDeque<ObjectId> = VecDeque::new();
            for id in &order {
                let t = times[id];
                if t > cut {
                    chosen.insert(*id, entry_time(t));
                    queue.push_back(*id);
                }
            }
            for id in recent {
                if let Some(t) = times.get(id) {
                    if !chosen.contains_key(id) {
                        chosen.insert(*id, entry_time((*t).max(floor)));
                        queue.push_back(*id);
                    }
                }
            }
            let mut visited: HashSet<ObjectId> = queue.iter().copied().collect();
            while let Some(id) = queue.pop_front() {
                for next in references(odb, &id) {
                    if !visited.insert(next) {
                        continue;
                    }
                    let Some(t) = times.get(&next) else {
                        // Kept elsewhere, or missing: not followed.
                        continue;
                    };
                    chosen
                        .entry(next)
                        .or_insert_with(|| entry_time((*t).max(floor)));
                    queue.push_back(next);
                }
            }
            chosen
        }
    };

    let ids: Vec<ObjectId> = order
        .into_iter()
        .filter(|id| chosen.contains_key(id))
        .collect();
    let described = pw::describe(odb, kind, &ids).map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(Selection {
        objects: pw::pack_order(&described),
        mtimes: chosen,
    })
}

/// The objects `id` refers to (parents and tree of a commit, entries of a
/// tree without gitlinks, the target of a tag). Nothing when it cannot be
/// read.
pub(crate) fn references(odb: &ObjectDatabase, id: &ObjectId) -> Vec<ObjectId> {
    let hash = odb.hash_kind();
    let Ok(Some(o)) = odb.read(id) else {
        return Vec::new();
    };
    match o.kind {
        ObjectKind::Commit => {
            let Ok(c) = Commit::parse(hash, &o.data) else {
                return Vec::new();
            };
            let mut out: Vec<ObjectId> = c.tree().into_iter().collect();
            out.extend(c.parents().unwrap_or_default());
            out
        }
        ObjectKind::Tree => TreeIter::new(hash, &o.data)
            .flatten()
            .filter(|e| e.mode_bits & 0o170000 != 0o160000)
            .map(|e| e.oid)
            .collect(),
        ObjectKind::Tag => Tag::parse(hash, &o.data)
            .and_then(|t| t.object())
            .into_iter()
            .collect(),
        ObjectKind::Blob => Vec::new(),
    }
}
