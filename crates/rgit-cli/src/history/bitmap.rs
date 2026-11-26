// SPDX-License-Identifier: MIT

//! `rev-list --use-bitmap-index` and `rev-list --test-bitmap`
//! (`Documentation/git-rev-list.adoc`).
//!
//! With a reachability bitmap, the objects reachable from the included
//! tips and not from the excluded ones are computed as bitmaps: a commit
//! with a stored bitmap contributes it, everything else is walked. Objects
//! outside the bitmapped pack (MIDX) get positions after its objects.
//! Output, as git's: the commits, trees, blobs and tags of the pack in
//! bit order, then the other objects in the order they were found; ids
//! only.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_pack::bitmap::write::{BuildError, Graph};
use rgit_pack::bitmap::{Bitmap, BitmapIndex};
use rgit_pack::write::ObjectFilter;

use super::options::Options;
use super::tips::Resolved;
use crate::messages::bitmap as msg;
use crate::prelude::*;

/// Whether the options leave a plain reachability query that bitmaps
/// answer; everything else is walked as usual.
pub fn eligible(opts: &Options, resolved: &Resolved) -> bool {
    let w = &opts.walk;
    opts.use_bitmap_index
        && resolved.paths.is_empty()
        && !opts.left_right
        && !opts.parents
        && !opts.children
        && !opts.timestamp
        && !opts.header
        && opts.pretty.is_none()
        && opts.pretty_arg.is_none()
        && opts.grep.is_empty()
        && opts.ancestry.is_empty()
        && !opts.bisect
        && !opts.bisect_vars
        && !opts.bisect_all
        && !opts.walk_reflogs
        && !opts.graph
        && !opts.maximal_only
        && opts.objects.is_none_or(|m| !m.edge && !m.in_commit_order)
        && !w.reverse
        && w.max_count.is_none()
        && w.skip == 0
        && !w.first_parent
        && w.max_age.is_none()
        && w.max_age_filter.is_none()
        && w.min_age.is_none()
        && w.min_parents.is_none()
        && w.max_parents.is_none()
        && !w.boundary
        && !w.ancestry_path
        && !w.left_only
        && !w.right_only
        && w.no_walk.is_none()
        && !w.cherry_pick
        && !w.cherry_mark
        && filters_eligible(opts)
        && resolved
            .commits
            .iter()
            .all(|c| c.2.is_none() || !opts.left_right)
}

/// Bit positions for the objects of the bitmap plus the objects found
/// outside of it.
struct Positions<'a> {
    index: &'a BitmapIndex,
    ext: Vec<(ObjectId, ObjectKind)>,
    ext_pos: HashMap<ObjectId, usize>,
}

impl Positions<'_> {
    fn bit(&mut self, id: &ObjectId, kind: ObjectKind) -> usize {
        if let Some(b) = self.index.position(id) {
            return b;
        }
        if let Some(&b) = self.ext_pos.get(id) {
            return b;
        }
        let b = self.index.num_objects() + self.ext.len();
        self.ext.push((*id, kind));
        self.ext_pos.insert(*id, b);
        b
    }
}

/// Everything reachable from `tips`.
fn reach(
    graph: &mut Graph<'_, rgit_odb::ObjectDatabase>,
    index: &BitmapIndex,
    pos: &mut Positions<'_>,
    tips: &[(ObjectId, ObjectKind)],
) -> Result<Bitmap, BuildError> {
    let mut bm = Bitmap::new();
    let mut stored = |id: &ObjectId| index.commit_bitmap(id).ok().flatten();
    let mut bit_of = |id: &ObjectId, kind: ObjectKind| Some(pos.bit(id, kind));
    graph.fill(tips, &mut bm, &mut stored, &mut bit_of)?;
    Ok(bm)
}

/// Answer the query with the bitmap; `None` when there is no usable
/// bitmap (the caller walks instead).
pub fn run(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    opts: &Options,
    resolved: &Resolved,
) -> Result<Option<i32>, Die> {
    let store = crate::repo_objects::Repo::open(ctx, setup.clone())?;
    let Some(index) = crate::bitmaps::open(store.local_packs()) else {
        return Ok(None);
    };
    let hash = setup.format.object_format;
    let mut graph = Graph::new(&store.odb, hash);
    let mut pos = Positions {
        index: &index,
        ext: Vec::new(),
        ext_pos: HashMap::new(),
    };
    let mut wants: Vec<(ObjectId, ObjectKind)> = Vec::new();
    let mut haves: Vec<(ObjectId, ObjectKind)> = Vec::new();
    for (id, excluded, _) in &resolved.commits {
        if *excluded {
            haves.push((*id, ObjectKind::Commit));
        } else {
            wants.push((*id, ObjectKind::Commit));
        }
    }
    let with_objects = opts.objects.is_some();
    for p in &resolved.pending {
        if with_objects || p.kind == ObjectKind::Commit {
            wants.push((p.id, p.kind));
        }
    }
    for (id, kind) in &resolved.excluded {
        haves.push((*id, *kind));
    }
    let Ok(mut result) = reach(&mut graph, &index, &mut pos, &wants) else {
        return Ok(None);
    };
    if !haves.is_empty() {
        // Which algorithm git would use for the excluded side; both give
        // the exact answer here.
        let label = if boundary_traversal(&store.config) {
            "haves/boundary"
        } else {
            "haves/classic"
        };
        let _region = rgit_trace::trace2::region("pack-bitmap", label, true);
        let Ok(h) = reach(&mut graph, &index, &mut pos, &haves) else {
            return Ok(None);
        };
        result.and_not(&h);
    }
    rgit_trace::trace2::data_int("bitmap", "bitmap/hits", 1, true);
    let reached = result.clone();
    if let Some(rules) = filter_rules(opts)? {
        // The objects named on the command line stay unless
        // `--filter-provided-objects` is given.
        let tips = if opts.filter_provided {
            Bitmap::new()
        } else {
            provided(&mut graph, &mut pos, &result, &wants, &rules)
        };
        let n = index.num_objects();
        let mut drop = Bitmap::new();
        for bit in result.ones() {
            if tips.get(bit) {
                continue;
            }
            let (id, kind) = if bit < n {
                let Some(kind) = index.kind_at(bit) else {
                    continue;
                };
                (index.id_at(bit), kind)
            } else {
                pos.ext[bit - n]
            };
            if rules.drops(kind, || {
                store.odb.header(&id).ok().flatten().map_or(0, |h| h.size)
            }) {
                drop.set(bit);
            }
        }
        result.and_not(&drop);
    }
    if opts.unpacked {
        // Everything in the bitmapped pack (MIDX) is packed; of the rest,
        // only loose objects stay.
        let n = index.num_objects();
        let mut keep = Bitmap::new();
        for (i, (id, _)) in pos.ext.iter().enumerate() {
            if !matches!(
                store.locate(id),
                Some(crate::repo_objects::Location::Packed(..))
            ) {
                keep.set(n + i);
            }
        }
        result.and(&keep);
    }

    let n = index.num_objects();
    let kinds: &[ObjectKind] = if with_objects {
        &[
            ObjectKind::Commit,
            ObjectKind::Tree,
            ObjectKind::Blob,
            ObjectKind::Tag,
        ]
    } else {
        &[ObjectKind::Commit]
    };
    let ext_wanted = |kind: ObjectKind| with_objects || kind == ObjectKind::Commit;

    let mut out = std::io::stdout().lock();
    if let Some(human) = opts.disk_usage {
        let mut total = 0u64;
        for bit in result.ones() {
            if bit < n {
                let kind = index.kind_at(bit);
                if kind.is_some_and(|k| kinds.contains(&k)) {
                    total += index.disk_size(bit);
                }
            } else if let Some((id, kind)) = pos.ext.get(bit - n)
                && ext_wanted(*kind)
            {
                total += store.disk_size(id);
            }
        }
        let text = if human {
            super::human_size(total)
        } else {
            total.to_string()
        };
        let _ = writeln!(out, "{text}");
        return Ok(Some(0));
    }
    if opts.count {
        let mut count = 0usize;
        for k in kinds {
            let mut t = index.type_bitmap(*k).clone();
            t.and(&result);
            count += t.count();
        }
        count += pos
            .ext
            .iter()
            .filter(|(id, kind)| ext_wanted(*kind) && result.get(pos.ext_pos[id]))
            .count();
        let _ = writeln!(out, "{count}");
        return Ok(Some(0));
    }
    let mut buf = Vec::new();
    for k in kinds {
        let mut t = index.type_bitmap(*k).clone();
        t.and(&result);
        for bit in t.ones() {
            buf.extend_from_slice(index.id_at(bit).to_hex().as_bytes());
            buf.push(b'\n');
        }
    }
    let pending: Vec<(ObjectId, ObjectKind)> = resolved
        .pending
        .iter()
        .filter(|p| with_objects || p.kind == ObjectKind::Commit)
        .map(|p| (p.id, p.kind))
        .collect();
    let commits: Vec<ObjectId> = wants
        .iter()
        .filter(|w| w.1 == ObjectKind::Commit)
        .map(|w| w.0)
        .collect();
    let order = ext_order(&mut graph, &pos, &reached, &commits, &pending);
    let mut listed = vec![false; pos.ext.len()];
    for i in order.into_iter().chain(0..pos.ext.len()) {
        if std::mem::replace(&mut listed[i], true) {
            continue;
        }
        let (id, kind) = pos.ext[i];
        if ext_wanted(kind) && result.get(n + i) {
            buf.extend_from_slice(id.to_hex().as_bytes());
            buf.push(b'\n');
        }
    }
    let _ = out.write_all(&buf);
    Ok(Some(0))
}

/// `rev-list --test-bitmap <commit>`: compare the stored bitmap of the
/// commit with a walk of everything reachable from it.
pub fn test_bitmap(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    resolved: &Resolved,
) -> Result<i32, Die> {
    let tips: Vec<(ObjectId, ObjectKind)> = resolved
        .commits
        .iter()
        .filter(|c| !c.1)
        .map(|c| (c.0, ObjectKind::Commit))
        .chain(resolved.pending.iter().map(|p| (p.id, p.kind)))
        .collect();
    if tips.len() != 1 {
        return Err(Die::Fatal(msg::TEST_ONE_COMMIT.into()));
    }
    let store = crate::repo_objects::Repo::open(ctx, setup.clone())?;
    let Some(index) = crate::bitmaps::open(store.local_packs()) else {
        return Err(Die::Fatal(msg::FAILED_TO_LOAD.into()));
    };
    let n = index.num_entries();
    eprintln!("{}", msg::test_header(n));
    let (tip, _) = tips[0];
    let stored: Option<Arc<Bitmap>> = match index.commit_bitmap(&tip) {
        Ok(b) => b,
        Err(e) => {
            crate::bitmaps::report(&e);
            None
        }
    };
    let Some(stored) = stored else {
        return Err(Die::Fatal(msg::no_indexed_bitmap(&tip.to_hex())));
    };
    eprintln!(
        "{}",
        msg::test_found(&tip.to_hex(), stored.words().len() * 64, checksum(&stored))
    );
    if let Some(p) = index.pack() {
        let name = p
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("pack-"))
            .unwrap_or_default()
            .to_owned();
        eprintln!("{}", msg::test_located_pack(&name));
    } else if let Some(m) = index.midx() {
        eprintln!("{}", msg::test_located_midx(&m.checksum().to_hex()));
    }
    // A plain walk, without using any stored bitmap.
    let hash = setup.format.object_format;
    let mut graph = Graph::new(&store.odb, hash);
    let mut pos = Positions {
        index: &index,
        ext: Vec::new(),
        ext_pos: HashMap::new(),
    };
    let mut walked = Bitmap::new();
    let mut none = |_: &ObjectId| None;
    let mut bit_of = |id: &ObjectId, kind: ObjectKind| Some(pos.bit(id, kind));
    graph
        .fill(
            &[(tip, ObjectKind::Commit)],
            &mut walked,
            &mut none,
            &mut bit_of,
        )
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let total = walked.count() as u64;
    let show =
        rgit_progress::stderr_is_terminal() || std::env::var_os("GIT_PROGRESS_DELAY").is_some();
    let mut meter = rgit_progress::Progress::start(msg::PROGRESS_VERIFYING, total, show);
    meter.set(total);
    meter.stop();
    let mut a = (*stored).clone();
    a.trim();
    walked.trim();
    if a != walked {
        return Err(Die::Fatal(msg::TEST_MISMATCH.into()));
    }
    // The type bitmaps must agree with the objects.
    for bit in walked.ones() {
        if bit >= index.num_objects() {
            return Err(Die::Fatal(msg::TEST_MISMATCH.into()));
        }
    }
    eprintln!("OK!");
    Ok(0)
}

/// The objects named on the command line that a filter keeps. Of the
/// trees and blobs outside the bitmap, as with git's walk of them, the
/// ones already found before their turn are not: the root trees of the
/// commits when the filter leaves out trees, and what an earlier named
/// tree holds (also one found before, when blobs are kept).
fn provided(
    graph: &mut Graph<'_, rgit_odb::ObjectDatabase>,
    pos: &mut Positions<'_>,
    reached: &Bitmap,
    wants: &[(ObjectId, ObjectKind)],
    rules: &FilterRules,
) -> Bitmap {
    let n = pos.index.num_objects();
    let mut tips = Bitmap::new();
    let mut seen = Bitmap::new();
    if rules.omits_trees() {
        for (i, (id, kind)) in pos.ext.clone().into_iter().enumerate() {
            if kind == ObjectKind::Commit
                && reached.get(n + i)
                && let Ok(t) = graph.commit_tree(&id)
            {
                seen.set(pos.bit(&t, ObjectKind::Tree));
            }
        }
    }
    for (id, kind) in wants {
        let bit = pos.bit(id, *kind);
        if *kind == ObjectKind::Commit || bit < n {
            tips.set(bit);
            continue;
        }
        if seen.get(bit) {
            if rules.keeps_blobs() {
                for link in graph.links(id, *kind).unwrap_or_default() {
                    mark(graph, pos, reached, &mut seen, link);
                }
            }
            continue;
        }
        tips.set(bit);
        mark(graph, pos, reached, &mut seen, (*id, *kind));
    }
    tips
}

/// Mark in `seen` what is reachable from `start` outside the bitmap.
fn mark(
    graph: &mut Graph<'_, rgit_odb::ObjectDatabase>,
    pos: &mut Positions<'_>,
    reached: &Bitmap,
    seen: &mut Bitmap,
    start: (ObjectId, ObjectKind),
) {
    let n = pos.index.num_objects();
    let mut stack = vec![start];
    while let Some((id, kind)) = stack.pop() {
        let b = pos.bit(&id, kind);
        if b < n || seen.get(b) || !reached.get(b) {
            continue;
        }
        seen.set(b);
        stack.extend(graph.links(&id, kind).unwrap_or_default());
    }
}

/// The objects outside the bitmap in the order git's walk finds them:
/// the commits by date, then the trees and blobs named on the command
/// line, then the trees of the commits, each tree before what it holds.
/// Indexes into `pos.ext`; objects that cannot be read are left to the
/// caller.
fn ext_order(
    graph: &mut Graph<'_, rgit_odb::ObjectDatabase>,
    pos: &Positions<'_>,
    reached: &Bitmap,
    commits: &[ObjectId],
    pending: &[(ObjectId, ObjectKind)],
) -> Vec<usize> {
    let n = pos.index.num_objects();
    let ext = |id: &ObjectId| pos.ext_pos.get(id).map(|b| *b - n);
    let mut done = vec![false; pos.ext.len()];
    let mut order = Vec::new();
    let mut queue = std::collections::BinaryHeap::new();
    let mut seq = 0u64;
    let mut push = |queue: &mut std::collections::BinaryHeap<_>,
                    graph: &Graph<'_, rgit_odb::ObjectDatabase>,
                    id: ObjectId| {
        let date = graph_date(&id, pos, graph);
        queue.push((date, std::cmp::Reverse(seq), id));
        seq += 1;
    };
    for c in commits {
        push(&mut queue, graph, *c);
    }
    let mut walked = Vec::new();
    while let Some((_, _, id)) = queue.pop() {
        let Some(i) = ext(&id) else { continue };
        if !reached.get(n + i) || std::mem::replace(&mut done[i], true) {
            continue;
        }
        order.push(i);
        walked.push(id);
        for p in graph.parents(&id).unwrap_or_default() {
            if ext(&p).is_some_and(|j| !done[j]) {
                push(&mut queue, graph, p);
            }
        }
    }
    let mut roots: Vec<(ObjectId, ObjectKind)> = pending.to_vec();
    for c in &walked {
        if let Ok(t) = graph.commit_tree(c) {
            roots.push((t, ObjectKind::Tree));
        }
    }
    for root in roots {
        let mut stack = vec![root];
        while let Some((id, kind)) = stack.pop() {
            let Some(i) = ext(&id) else { continue };
            if !reached.get(n + i) || std::mem::replace(&mut done[i], true) {
                continue;
            }
            order.push(i);
            let links = graph.links(&id, kind).unwrap_or_default();
            stack.extend(links.into_iter().rev());
        }
    }
    order
}

/// The committer date of an object outside the bitmap (0 if unknown).
fn graph_date(
    id: &ObjectId,
    pos: &Positions<'_>,
    graph: &Graph<'_, rgit_odb::ObjectDatabase>,
) -> i64 {
    if pos.index.position(id).is_some() {
        return 0;
    }
    graph.commit_date(id).unwrap_or(0)
}

/// What `--filter` leaves out of a bitmap result, combined.
#[derive(Default)]
struct FilterRules {
    /// Blobs of at least this size are left out (0: all blobs).
    blob_limit: Option<u64>,
    /// No trees and no blobs (`tree:0`).
    no_trees: bool,
    /// Only objects of these types (every `object:type` filter).
    kinds: Vec<ObjectKind>,
}

impl FilterRules {
    fn add(&mut self, f: &ObjectFilter) -> bool {
        match f {
            ObjectFilter::BlobNone => self.blob_limit = Some(0),
            ObjectFilter::BlobLimit(n) => {
                self.blob_limit = Some(self.blob_limit.map_or(*n, |m| m.min(*n)));
            }
            ObjectFilter::TreeDepth(0) => self.no_trees = true,
            ObjectFilter::ObjectType(k) => self.kinds.push(*k),
            ObjectFilter::Combine(subs) => return subs.iter().all(|s| self.add(s)),
            ObjectFilter::TreeDepth(_) | ObjectFilter::SparseOid(_) => return false,
        }
        true
    }

    /// Whether objects of `kind` are kept at all.
    fn allows(&self, kind: ObjectKind) -> bool {
        self.kinds.iter().all(|k| *k == kind)
    }

    /// Whether some blobs are kept.
    fn keeps_blobs(&self) -> bool {
        !self.no_trees && self.allows(ObjectKind::Blob) && self.blob_limit != Some(0)
    }

    /// Whether no tree is kept.
    fn omits_trees(&self) -> bool {
        self.no_trees || !self.allows(ObjectKind::Tree)
    }

    /// Whether an object of `kind` is left out; `size` is asked for blobs
    /// only.
    fn drops(&self, kind: ObjectKind, size: impl FnOnce() -> u64) -> bool {
        if self.kinds.iter().any(|k| *k != kind) {
            return true;
        }
        match kind {
            ObjectKind::Tree => self.no_trees,
            ObjectKind::Blob => {
                self.no_trees
                    || self
                        .blob_limit
                        .is_some_and(|limit| limit == 0 || size() >= limit)
            }
            _ => false,
        }
    }
}

/// The filter of `--filter` as rules on bitmaps; `None` without a filter.
fn filter_rules(opts: &Options) -> Result<Option<FilterRules>, Die> {
    let Some((filter, _)) = crate::promisor::parse_filters(&opts.filters)? else {
        return Ok(None);
    };
    let mut rules = FilterRules::default();
    // `eligible` let only filters through that bitmaps can answer.
    let _ = rules.add(&filter);
    Ok(Some(rules))
}

/// Whether every `--filter` can be answered with bitmaps: `blob:none`,
/// `blob:limit`, `tree:0` and `object:type`, also combined. Others
/// (`tree:<n>` deeper than 0, `sparse:oid`) walk as usual.
fn filters_eligible(opts: &Options) -> bool {
    match crate::promisor::parse_filters(&opts.filters) {
        Ok(None) => true,
        Ok(Some((f, _))) => FilterRules::default().add(&f),
        // The walk reports the error.
        Err(_) => false,
    }
}

/// `pack.useBitmapBoundaryTraversal` (default: `feature.experimental`),
/// overridden by `GIT_TEST_PACK_USE_BITMAP_BOUNDARY_TRAVERSAL`.
fn boundary_traversal(config: &rgit_config::ConfigSet) -> bool {
    if let Ok(v) = std::env::var("GIT_TEST_PACK_USE_BITMAP_BOUNDARY_TRAVERSAL") {
        return !matches!(v.as_str(), "0" | "false" | "no" | "off" | "");
    }
    config
        .bool("pack.usebitmapboundarytraversal")
        .ok()
        .flatten()
        .or_else(|| config.bool("feature.experimental").ok().flatten())
        .unwrap_or(false)
}

/// A checksum of the words of a bitmap for the `--test-bitmap` report.
fn checksum(b: &Bitmap) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for w in b.words() {
        for byte in w.to_le_bytes() {
            h = (h ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        }
    }
    h
}
