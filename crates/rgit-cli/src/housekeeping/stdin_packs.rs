// SPDX-License-Identifier: MIT
//! The object selection of `git pack-objects --stdin-packs[=follow]`
//! (Documentation/git-pack-objects.adoc).
//!
//! The input names packs of the repository or its alternates by file name
//! (`pack-<hash>.pack`): without prefix the packs to include, with `^` the
//! packs to exclude, and in `follow` mode with `!` packs that are excluded
//! but not closed under reachability. The names are looked up in sorted
//! order, so an unknown name is reported the way git reports it.
//!
//! The result is every object of an included pack that no excluded pack
//! has, with `--unpacked` also the loose objects that no excluded pack
//! has. In `follow` mode the objects reachable from those (and from the
//! objects of `!` packs) are added when they live in packs that were not
//! named. Without `!` packs the walk passes through excluded objects;
//! with them, `^` packs end the walk. Missing objects are skipped.

use std::collections::{HashSet, VecDeque};
use std::io::BufRead;
use std::sync::Arc;

use rgit_hash::ObjectId;
use rgit_odb::ObjectDatabase;
use rgit_pack::Pack;
use rgit_pack::write::{self as pw, ListedObject};

use super::store;
use crate::Die;
use crate::messages::housekeeping as msg;

/// How a listed pack is used.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Included,
    /// `^`: excluded, closed under reachability.
    Excluded,
    /// `!` (follow mode): excluded, the walk goes on through it.
    ExcludedOpen,
}

/// The packs of the repository and of its alternates.
fn all_packs(odb: &ObjectDatabase) -> Vec<(String, Arc<Pack>)> {
    let named = |pack: Arc<Pack>| {
        let name = pack
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        (name, pack)
    };
    let mut out: Vec<(String, Arc<Pack>)> = store::local_packs(odb, &[])
        .into_iter()
        .map(|p| named(p.pack))
        .collect();
    let objects_dir = odb.loose().objects_dir();
    for alt in rgit_odb::resolve_alternates(objects_dir) {
        let alt_store = rgit_odb::PackStore::open(alt.join("pack"), odb.hash_kind());
        out.extend(alt_store.set().packs().into_iter().map(named));
    }
    out
}

/// Choose the objects for `pack-objects --stdin-packs`, read from `input`.
/// `follow`: `--stdin-packs=follow`; `unpacked`: `--unpacked`.
pub fn select(
    odb: &ObjectDatabase,
    input: &mut dyn BufRead,
    follow: bool,
    unpacked: bool,
    exclude_promisor: bool,
) -> Result<Vec<ListedObject>, Die> {
    let mut lines: Vec<(Role, String)> = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
        let text = line.trim_end_matches(['\n', '\r']);
        if text.is_empty() {
            continue;
        }
        let entry = if let Some(name) = text.strip_prefix('^') {
            (Role::Excluded, name)
        } else if let Some(name) = text.strip_prefix('!').filter(|_| follow) {
            (Role::ExcludedOpen, name)
        } else {
            (Role::Included, text)
        };
        lines.push((entry.0, entry.1.to_string()));
    }
    lines.sort_by(|a, b| a.1.cmp(&b.1));

    let packs = all_packs(odb);
    let mut listed: Vec<(Role, Arc<Pack>)> = Vec::new();
    for (role, name) in &lines {
        let Some((_, pack)) = packs.iter().find(|(n, _)| n == name) else {
            return Err(Die::Fatal(msg::could_not_find_pack(name)));
        };
        if exclude_promisor && *role == Role::Included && pack.is_promisor() {
            return Err(Die::Fatal(msg::promisor_pack_excluded(
                &pack.path().display().to_string(),
            )));
        }
        listed.push((*role, pack.clone()));
    }
    let any_open = listed.iter().any(|(r, _)| *r == Role::ExcludedOpen);
    let in_role =
        |id: &ObjectId, role: Role| listed.iter().any(|(r, p)| *r == role && p.contains(id));
    let excluded = |id: &ObjectId| in_role(id, Role::Excluded) || in_role(id, Role::ExcludedOpen);

    // The included objects, pack by pack in offset order.
    let mut chosen: Vec<ObjectId> = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut starts: Vec<ObjectId> = Vec::new();
    for (role, pack) in &listed {
        if *role == Role::Excluded {
            continue;
        }
        for id in ids_in_offset_order(pack) {
            if *role == Role::Included && !excluded(&id) && seen.insert(id) {
                chosen.push(id);
            }
            if *role == Role::ExcludedOpen {
                starts.push(id);
            }
        }
    }
    if unpacked {
        let mut loose = store::loose_objects(odb.loose().objects_dir(), odb.hash_kind());
        loose.sort_by_key(|a| a.0);
        for (id, _, _) in loose {
            if !excluded(&id) && seen.insert(id) {
                chosen.push(id);
            }
        }
    }

    // `--exclude-promisor-objects`: the follow walk stops at the objects
    // of promisor packs.
    let promisor = if exclude_promisor && follow {
        odb.promisor_pack_objects()
    } else {
        HashSet::new()
    };
    if follow {
        let mut visited: HashSet<ObjectId> = chosen.iter().chain(&starts).copied().collect();
        let mut queue: VecDeque<ObjectId> = visited.iter().copied().collect();
        // A stable walk: start from the chosen objects in order.
        queue.make_contiguous().sort();
        while let Some(id) = queue.pop_front() {
            for next in super::cruft::references(odb, &id) {
                if !visited.insert(next) {
                    continue;
                }
                if any_open && in_role(&next, Role::Excluded) {
                    continue;
                }
                if !odb.contains(&next) || promisor.contains(&next) {
                    continue;
                }
                if !excluded(&next) && seen.insert(next) {
                    chosen.push(next);
                }
                queue.push_back(next);
            }
        }
    }

    let described =
        pw::describe(odb, odb.hash_kind(), &chosen).map_err(|e| Die::Fatal(e.to_string()))?;
    Ok(pw::pack_order(&described))
}

/// The ids of `pack` in the order of their offsets.
fn ids_in_offset_order(pack: &Pack) -> Vec<ObjectId> {
    let index = pack.index();
    let mut by_offset: Vec<(u64, u32)> = (0..index.len())
        .filter_map(|pos| index.offset_at(pos).ok().map(|off| (off, pos)))
        .collect();
    by_offset.sort_unstable();
    by_offset
        .into_iter()
        .map(|(_, pos)| index.id_at(pos))
        .collect()
}
