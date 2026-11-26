// SPDX-License-Identifier: MIT
//! Shallow fetches: which commits the client gets without their parents
//! (`shallow <oid>`), which of its shallow commits become complete
//! (`unshallow <oid>`), and what `pack-objects` has to be told.
//!
//! `Documentation/gitprotocol-v2.adoc` (the `shallow-info` section) and
//! `Documentation/technical/shallow.adoc`; the cases the documentation
//! leaves open were observed from git 2.56:
//!
//! - `deepen <n>`: the commits at distance `n` from the wanted commits
//!   are shallow, also a root commit at that distance.
//! - `deepen-relative`: the depth is added to the distance of the
//!   nearest shallow commit of the client from the wanted commits;
//!   without one, the fetch is not shallow.
//! - `deepen-since`/`deepen-not`: a commit is shallow when one of its
//!   parents is cut off; no commit left is an error.
//! - A repository that is shallow itself reports its shallow commits
//!   that the wanted commits reach.
//! - `pack-objects` gets `--shallow <oid>` for the client's shallow
//!   commits and the new ones, and the parents of the commits that
//!   become complete as additional tips.
//! - The order of the `shallow` lines: for a depth, the reverse of the
//!   order in which a depth-first walk finds them (see [`by_depth`]);
//!   for `deepen-since` and `deepen-not`, newest commit first, as
//!   `rev-list` walks. `unshallow` lines follow the client's `shallow`
//!   lines.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use rgit_hash::ObjectId;

use super::graph::Graph;
use super::{Error, Result, messages as msg};

/// The shallow arguments of a `fetch` request.
#[derive(Debug, Default, Clone)]
pub(crate) struct ShallowRequest {
    /// `shallow <oid>`: the client's shallow commits.
    pub client: Vec<ObjectId>,
    /// `deepen <n>`.
    pub depth: Option<u64>,
    /// `deepen-relative`.
    pub relative: bool,
    /// `deepen-since <timestamp>`.
    pub since: Option<i64>,
    /// `deepen-not <ref>`, resolved to commits.
    pub not: Vec<ObjectId>,
    /// A `deepen-not` argument was given.
    pub has_not: bool,
}

/// What follows from a [`ShallowRequest`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ShallowPlan {
    /// Send a `shallow-info` section (possibly without lines).
    pub section: bool,
    /// `shallow <oid>` lines.
    pub shallow: Vec<ObjectId>,
    /// `unshallow <oid>` lines.
    pub unshallow: Vec<ObjectId>,
    /// `--shallow <oid>` lines for `pack-objects`.
    pub pack_shallow: Vec<ObjectId>,
    /// Tips for `pack-objects` besides the wanted objects.
    pub extra_wants: Vec<ObjectId>,
}

/// Work out the plan for a request that wants `wants` from a repository
/// with the shallow commits `server_shallow` (which `graph` cuts off).
pub(crate) fn plan(
    graph: &mut Graph<'_>,
    server_shallow: &[ObjectId],
    req: &ShallowRequest,
    wants: &[ObjectId],
) -> Result<ShallowPlan> {
    let mut seen = HashSet::new();
    let client: Vec<ObjectId> = req
        .client
        .iter()
        .filter(|id| seen.insert(**id) && graph.commit(id).is_some())
        .copied()
        .collect();
    let client_set: HashSet<ObjectId> = client.iter().copied().collect();
    let mut seen = HashSet::new();
    let heads: Vec<ObjectId> = wants
        .iter()
        .filter_map(|id| graph.peel_to_commit(id))
        .filter(|id| seen.insert(*id))
        .collect();

    // The commits the client ends up with, in walk order, and which of
    // them it has without parents.
    let (reached, boundary): (Vec<ObjectId>, HashSet<ObjectId>) = if let Some(depth) = req.depth {
        let depth = if req.relative {
            // Deeper than what the client has now.
            match nearest(graph, &heads, &client_set) {
                Some(distance) => depth.saturating_add(distance),
                None => {
                    return Ok(ShallowPlan {
                        section: true,
                        ..ShallowPlan::default()
                    });
                }
            }
        } else {
            depth
        };
        by_depth(graph, &heads, depth, &HashSet::new())
    } else if req.since.is_some() || req.has_not {
        let found = by_revs(graph, &heads, req.since, &req.not);
        if found.0.is_empty() {
            return Err(Error::fatal(msg::NO_COMMITS_SELECTED));
        }
        found
    } else if !server_shallow.is_empty() {
        // Not a shallow request: the client keeps its shallow commits and
        // learns which commits of this repository have no parents here.
        let (reached, _) = by_depth(graph, &heads, u64::MAX, &client_set);
        let shallow: Vec<ObjectId> = reached
            .iter()
            .filter(|id| graph.is_graft(id) && !client_set.contains(id))
            .copied()
            .collect();
        let mut pack_shallow = client.clone();
        pack_shallow.extend(server_shallow.iter().filter(|id| !client_set.contains(id)));
        return Ok(ShallowPlan {
            section: true,
            shallow,
            unshallow: Vec::new(),
            pack_shallow,
            extra_wants: Vec::new(),
        });
    } else {
        return Ok(ShallowPlan {
            section: !req.client.is_empty(),
            pack_shallow: client,
            ..ShallowPlan::default()
        });
    };

    let reached_set: HashSet<ObjectId> = reached.iter().copied().collect();
    let shallow: Vec<ObjectId> = reached
        .iter()
        .filter(|id| boundary.contains(id) && !client_set.contains(id))
        .copied()
        .collect();
    let unshallow: Vec<ObjectId> = client
        .iter()
        .filter(|id| reached_set.contains(id) && !boundary.contains(id))
        .copied()
        .collect();
    let mut pack_shallow = client.clone();
    let mut listed = client_set.clone();
    for id in reached.iter().filter(|id| boundary.contains(id)) {
        if listed.insert(*id) {
            pack_shallow.push(*id);
        }
    }
    for id in server_shallow {
        if listed.insert(*id) {
            pack_shallow.push(*id);
        }
    }
    // The client has a commit that becomes complete, but not what is
    // behind it.
    let mut extra_wants = Vec::new();
    for id in &unshallow {
        if let Some(c) = graph.commit(id) {
            extra_wants.extend(c.parents.iter().copied());
        }
    }
    Ok(ShallowPlan {
        section: true,
        shallow,
        unshallow,
        pack_shallow,
        extra_wants,
    })
}

/// The distance (a head counts as 1) of the nearest commit in `targets`
/// from `heads`.
fn nearest(graph: &mut Graph<'_>, heads: &[ObjectId], targets: &HashSet<ObjectId>) -> Option<u64> {
    let mut seen: HashSet<ObjectId> = heads.iter().copied().collect();
    let mut queue: VecDeque<(ObjectId, u64)> = heads.iter().map(|id| (*id, 1)).collect();
    while let Some((id, distance)) = queue.pop_front() {
        if targets.contains(&id) {
            return Some(distance);
        }
        let Some(c) = graph.commit(&id) else { continue };
        for p in &c.parents {
            if seen.insert(*p) {
                queue.push_back((*p, distance + 1));
            }
        }
    }
    None
}

/// The commits up to `depth` steps from `heads` (a head counts as 1),
/// not looking behind the commits in `stop`, and those of them that have
/// no parents for the client: the ones at `depth` and the ones this
/// repository has shallowly.
///
/// The walk is depth first, one head after the other, the last parent of
/// a commit first; a commit found again on a shorter way is walked again.
/// The result lists the commits without parents first, the one found last
/// at the front (the order of git's `shallow` lines; a commit can be in
/// it more than once), then the others.
fn by_depth(
    graph: &mut Graph<'_>,
    heads: &[ObjectId],
    depth: u64,
    stop: &HashSet<ObjectId>,
) -> (Vec<ObjectId>, HashSet<ObjectId>) {
    let mut depth_of: HashMap<ObjectId, u64> = HashMap::new();
    let mut found: Vec<ObjectId> = Vec::new();
    let mut ends: Vec<ObjectId> = Vec::new();
    for head in heads {
        // A head that an earlier head reached is walked again from 1.
        if depth_of.get(head).is_some_and(|d| *d <= 1) || graph.commit(head).is_none() {
            continue;
        }
        depth_of.insert(*head, 1);
        let mut stack = vec![*head];
        while let Some(id) = stack.pop() {
            let d = depth_of[&id];
            found.push(id);
            if d >= depth || graph.is_graft(&id) {
                ends.push(id);
                continue;
            }
            if stop.contains(&id) {
                continue;
            }
            let parents = graph
                .commit(&id)
                .map(|c| c.parents.clone())
                .unwrap_or_default();
            for p in parents {
                let shorter = depth_of.get(&p).is_none_or(|old| *old > d + 1);
                if shorter && graph.commit(&p).is_some() {
                    depth_of.insert(p, d + 1);
                    stack.push(p);
                }
            }
        }
    }
    // A commit at the depth limit may have been found again nearer by.
    let is_end = |id: &ObjectId| depth_of[id] >= depth || graph.is_graft(id);
    // A commit this repository has shallowly is listed once for every
    // time the walk came to it, as git does.
    let mut reached: Vec<ObjectId> = ends.iter().rev().filter(|id| is_end(id)).copied().collect();
    let boundary: HashSet<ObjectId> = reached.iter().copied().collect();
    let mut seen = boundary.clone();
    reached.extend(found.iter().filter(|id| seen.insert(**id)));
    (reached, boundary)
}

/// `rev-list <heads> --max-age=<since> --not <not>`: the commits reached
/// from `heads` without passing a commit that is older than `since` or
/// reachable from `not`, and those of them with a parent that is left
/// out (or that this repository has shallowly).
fn by_revs(
    graph: &mut Graph<'_>,
    heads: &[ObjectId],
    since: Option<i64>,
    not: &[ObjectId],
) -> (Vec<ObjectId>, HashSet<ObjectId>) {
    let excluded: HashSet<ObjectId> = reachable(graph, not, &HashSet::new()).into_iter().collect();
    let mut included: HashSet<ObjectId> = HashSet::new();
    let mut order = Vec::new();
    let mut seen: HashSet<ObjectId> = HashSet::new();
    // Newest first; commits of the same time in the order they were found.
    let mut queue: BinaryHeap<(i64, std::cmp::Reverse<u64>, ObjectId)> = BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |graph: &mut Graph<'_>, queue: &mut BinaryHeap<_>, id: ObjectId| {
        if seen.insert(id) {
            if let Some(c) = graph.commit(&id) {
                queue.push((c.time, std::cmp::Reverse(seq), id));
                seq += 1;
            }
        }
    };
    for id in heads {
        push(graph, &mut queue, *id);
    }
    while let Some((time, _, id)) = queue.pop() {
        if excluded.contains(&id) || since.is_some_and(|t| time < t) {
            continue;
        }
        included.insert(id);
        order.push(id);
        let parents = graph
            .commit(&id)
            .map(|c| c.parents.clone())
            .unwrap_or_default();
        for p in parents {
            push(graph, &mut queue, p);
        }
    }
    let mut boundary = HashSet::new();
    for id in &order {
        let cut = graph.is_graft(id)
            || graph
                .commit(id)
                .is_some_and(|c| c.parents.iter().any(|p| !included.contains(p)));
        if cut {
            boundary.insert(*id);
        }
    }
    (order, boundary)
}

/// The commits reachable from `heads`, not looking behind the commits in
/// `stop` (which are part of the result).
fn reachable(graph: &mut Graph<'_>, heads: &[ObjectId], stop: &HashSet<ObjectId>) -> Vec<ObjectId> {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut order = Vec::new();
    let mut queue: VecDeque<ObjectId> = heads.iter().copied().collect();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let Some(c) = graph.commit(&id) else { continue };
        order.push(id);
        if !stop.contains(&id) {
            queue.extend(c.parents.iter().copied());
        }
    }
    order
}
