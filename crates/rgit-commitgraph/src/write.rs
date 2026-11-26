// SPDX-License-Identifier: MIT
//! Writing commit-graph files (`git commit-graph write`).
//!
//! The caller collects the starting commits (refs, standard input, packs);
//! [`write`] adds their ancestors, decides about split layers
//! (`Documentation/technical/commit-graph.adoc`, "Merge Strategy"),
//! computes topological levels, corrected commit dates and changed-path
//! filters and writes the file. The chunk order and every value are those
//! of git's files (compared byte for byte in the tests).

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rgit_hash::{HashKind, Hasher, ObjectId};
use rgit_object::TreeIter;
use rgit_progress::Progress;
use rgit_trace::trace2;

use crate::bloom::{self, Built};
use crate::file::{hash_version, id};
use crate::graph::{Graph, Source, chain_dir, chain_path, layer_path, single_path};
use crate::messages as msg;
use crate::{GENERATION_V1_MAX, NO_PARENT, Objects, ParsedCommit, SIGNATURE};

/// `--split[=<strategy>]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// Bare `--split`: merge layers by the size rules.
    Merge,
    /// `--split=no-merge`.
    NoMerge,
    /// `--split=replace`.
    Replace,
}

/// Options of a write.
#[derive(Debug, Clone)]
pub struct WriteOptions {
    /// Repository hash.
    pub hash: HashKind,
    /// Write a chain layer.
    pub split: Option<Split>,
    /// `--size-multiple` (default 2).
    pub size_multiple: u32,
    /// `--max-commits` (0: no limit).
    pub max_commits: u32,
    /// `--expire-time` in seconds since the epoch (`None`: now).
    pub expire_time: Option<i64>,
    /// `--append`: keep the commits of the existing graph.
    pub append: bool,
    /// `--[no-]changed-paths`; `None` decides from the configuration and
    /// the existing graph.
    pub changed_paths: Option<bool>,
    /// `commitGraph.changedPaths`.
    pub changed_paths_config: bool,
    /// `--max-new-filters` (`None`: no limit).
    pub max_new_filters: Option<u32>,
    /// `commitGraph.generationVersion`.
    pub generation_version: u32,
    /// `commitGraph.changedPathsVersion`.
    pub changed_paths_version: i32,
    /// `GIT_TEST_BLOOM_SETTINGS_*` overrides.
    pub bloom_num_hashes: Option<u32>,
    /// See [`Self::bloom_num_hashes`].
    pub bloom_bits_per_entry: Option<u32>,
    /// See [`Self::bloom_num_hashes`].
    pub bloom_max_changed_paths: Option<u32>,
    /// Show progress.
    pub progress: bool,
    /// Permission bits of the written files.
    pub file_mode: u32,
}

impl WriteOptions {
    /// Defaults for `hash`.
    pub fn new(hash: HashKind) -> WriteOptions {
        WriteOptions {
            hash,
            split: None,
            size_multiple: 2,
            max_commits: 0,
            expire_time: None,
            append: false,
            changed_paths: None,
            changed_paths_config: false,
            max_new_filters: None,
            generation_version: 2,
            changed_paths_version: -1,
            bloom_num_hashes: None,
            bloom_bits_per_entry: None,
            bloom_max_changed_paths: None,
            progress: false,
            file_mode: 0o444,
        }
    }
}

/// Why a write failed.
#[derive(Debug)]
pub enum WriteError {
    /// A commit could not be read or parsed (git: `unable to parse commit
    /// <id>`).
    UnparsableCommit(ObjectId),
    /// Reading the existing graph failed.
    Graph(crate::ReadError),
    /// File system error.
    Io(io::Error),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::UnparsableCommit(id) => write!(f, "unable to parse commit {}", id.to_hex()),
            WriteError::Graph(e) => e.fmt(f),
            WriteError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> WriteError {
        WriteError::Io(e)
    }
}

impl From<crate::ReadError> for WriteError {
    fn from(e: crate::ReadError) -> WriteError {
        WriteError::Graph(e)
    }
}

/// What a write did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WriteReport {
    /// Commits in the written file (0: nothing written).
    pub written: u32,
    /// Filters computed.
    pub filters_computed: u32,
    /// Filters taken over or skipped.
    pub filters_not_computed: u32,
}

/// One commit to write.
struct Entry {
    id: ObjectId,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    date: u64,
}

/// Where a parent lives.
#[derive(Clone, Copy)]
enum ParentRef {
    /// Index in the new file.
    New(usize),
    /// Global position in the kept layers.
    Base(u32),
}

/// Choose the Bloom settings to write with. `None`: no filters.
fn bloom_settings(opts: &WriteOptions, existing: Option<&Graph>) -> Option<bloom::Settings> {
    let existing_settings = existing.and_then(Graph::top_bloom_settings);
    let want = match opts.changed_paths {
        Some(b) => b,
        None => opts.changed_paths_config || existing_settings.is_some(),
    };
    if !want {
        return None;
    }
    let mut s = bloom::Settings::default();
    if let Some(e) = existing_settings {
        s.num_hashes = e.num_hashes;
        s.bits_per_entry = e.bits_per_entry;
    } else {
        if let Some(n) = opts.bloom_num_hashes {
            s.num_hashes = n;
        }
        if let Some(n) = opts.bloom_bits_per_entry {
            s.bits_per_entry = n;
        }
    }
    if let Some(n) = opts.bloom_max_changed_paths {
        s.max_changed_paths = n;
    }
    s.hash_version = match opts.changed_paths_version {
        -1 => existing_settings.map_or(1, |e| e.hash_version),
        2 => 2,
        _ => 1,
    };
    Some(s)
}

/// Changed paths of `tree` against `parent` (none: the empty tree), with
/// every leading directory.
pub fn changed_paths(
    objects: &dyn Objects,
    hash: HashKind,
    tree: &ObjectId,
    parent: Option<&ObjectId>,
) -> Vec<Vec<u8>> {
    let mut out = HashSet::new();
    let mut prefix = Vec::new();
    diff_trees(objects, hash, parent, Some(tree), &mut prefix, &mut out);
    let mut v: Vec<Vec<u8>> = out.into_iter().collect();
    v.sort();
    v
}

struct TreeEnt {
    name: Vec<u8>,
    mode: u32,
    oid: ObjectId,
}

impl TreeEnt {
    fn is_tree(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }
}

fn entries(objects: &dyn Objects, hash: HashKind, id: Option<&ObjectId>) -> Vec<TreeEnt> {
    let Some(id) = id else {
        return Vec::new();
    };
    let Some(data) = objects.tree(id) else {
        return Vec::new();
    };
    TreeIter::new(hash, &data)
        .map_while(Result::ok)
        .map(|e| TreeEnt {
            name: e.name.to_vec(),
            mode: e.mode_bits,
            oid: e.oid,
        })
        .collect()
}

fn add_path(out: &mut HashSet<Vec<u8>>, path: &[u8]) {
    for (i, &b) in path.iter().enumerate() {
        if b == b'/' {
            out.insert(path[..i].to_vec());
        }
    }
    out.insert(path.to_vec());
}

fn diff_trees(
    objects: &dyn Objects,
    hash: HashKind,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
    prefix: &mut Vec<u8>,
    out: &mut HashSet<Vec<u8>>,
) {
    let a = entries(objects, hash, old);
    let b = entries(objects, hash, new);
    let (mut i, mut j) = (0, 0);
    let len = prefix.len();
    let one_side = |e: &TreeEnt, prefix: &mut Vec<u8>, out: &mut HashSet<Vec<u8>>, old: bool| {
        prefix.extend_from_slice(&e.name);
        if e.is_tree() {
            prefix.push(b'/');
            if old {
                diff_trees(objects, hash, Some(&e.oid), None, prefix, out);
            } else {
                diff_trees(objects, hash, None, Some(&e.oid), prefix, out);
            }
            prefix.pop();
        } else {
            add_path(out, prefix);
        }
        prefix.truncate(len);
    };
    while i < a.len() || j < b.len() {
        let ord = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => {
                rgit_object::entry_cmp(&x.name, x.is_tree(), &y.name, y.is_tree())
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Greater,
        };
        match ord {
            std::cmp::Ordering::Less => {
                one_side(&a[i], prefix, out, true);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                one_side(&b[j], prefix, out, false);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                let (x, y) = (&a[i], &b[j]);
                if x.oid != y.oid || x.mode != y.mode {
                    prefix.extend_from_slice(&x.name);
                    if x.is_tree() && y.is_tree() {
                        prefix.push(b'/');
                        diff_trees(objects, hash, Some(&x.oid), Some(&y.oid), prefix, out);
                        prefix.pop();
                    } else {
                        add_path(out, prefix);
                    }
                    prefix.truncate(len);
                }
                i += 1;
                j += 1;
            }
        }
    }
}

fn progress(title: &str, total: u64, show: bool) -> Option<Progress> {
    show.then(|| Progress::start_delayed(title, total, true))
}

fn read_commit(objects: &dyn Objects, id: &ObjectId) -> Result<ParsedCommit, WriteError> {
    match objects.commit(id) {
        Ok(Some(c)) => Ok(c),
        _ => Err(WriteError::UnparsableCommit(*id)),
    }
}

/// Write the commit-graph of `object_dir` from `start` (commit ids,
/// deduplicated by the caller or not) and their ancestors. `existing` is
/// the graph loaded for this repository (with Bloom filters when they may
/// be reused). Returns what was written; nothing is written when there are
/// no commits (or, for a split write, no new ones).
pub fn write(
    objects: &dyn Objects,
    object_dir: &Path,
    existing: Option<&Graph>,
    start: &[ObjectId],
    opts: &WriteOptions,
) -> Result<WriteReport, WriteError> {
    let hash = opts.hash;
    let settings = bloom_settings(opts, existing);
    let show = opts.progress;

    // Layers a split write may build on: a chain, or the repository's own
    // single file. A single file of an alternate is not a base.
    let base_graph = match (opts.split, existing) {
        (Some(Split::Replace) | None, _) | (_, None) => None,
        (Some(_), Some(g)) => match g.source() {
            Source::Chain(_) => Some(g),
            Source::Single if same_dir(g.object_dir(), object_dir) => Some(g),
            Source::Single => None,
        },
    };
    if opts.split.is_some() {
        std::fs::create_dir_all(chain_dir(object_dir))?;
    }

    // Starting commits.
    let mut inputs: Vec<ObjectId> = start.to_vec();
    if opts.append {
        if let Some(g) = existing {
            for pos in 0..g.num_commits() {
                if let Some(id) = g.id_at(pos) {
                    inputs.push(id);
                }
            }
        }
    }
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut list: Vec<ObjectId> = Vec::new();
    {
        let mut p = progress(msg::LOADING_KNOWN, inputs.len() as u64, show);
        for (i, id) in inputs.iter().enumerate() {
            if seen.insert(*id) {
                list.push(*id);
            }
            if let Some(p) = p.as_mut() {
                p.set(i as u64 + 1);
            }
        }
        if let Some(p) = p {
            p.stop();
        }
    }
    // Ancestors; a split write stops at commits of the base layers.
    let in_base = |id: &ObjectId| base_graph.is_some_and(|g| g.lookup(id).is_some());
    let mut parsed: HashMap<ObjectId, ParsedCommit> = HashMap::new();
    {
        let mut p = progress(msg::EXPANDING_REACHABLE, 0, show);
        let mut i = 0;
        while i < list.len() {
            let id = list[i];
            i += 1;
            if let Some(p) = p.as_mut() {
                p.set(i as u64);
            }
            if in_base(&id) {
                continue;
            }
            let c = read_commit(objects, &id)?;
            for parent in &c.parents {
                if seen.insert(*parent) {
                    list.push(*parent);
                }
            }
            parsed.insert(id, c);
        }
        if let Some(p) = p {
            p.stop();
        }
    }
    for title in [msg::CLEARING_MARKS, msg::FINDING_EXTRA_EDGES] {
        let mut p = progress(title, list.len() as u64, show);
        if let Some(p) = p.as_mut() {
            for i in 0..list.len() {
                p.set(i as u64 + 1);
            }
        }
        if let Some(p) = p {
            p.stop();
        }
    }
    let mut new: Vec<ObjectId> = list.into_iter().filter(|id| !in_base(id)).collect();

    // Split: which layers stay below the new one.
    let mut kept_layers = base_graph.map_or(0, |g| g.layers().len());
    if let (Some(g), Some(split)) = (base_graph, opts.split) {
        if new.is_empty() {
            return Ok(WriteReport::default());
        }
        let mut n = new.len() as u64;
        if split == Split::Merge {
            while kept_layers > 0 {
                let top = &g.layers()[kept_layers - 1];
                if !same_dir(&top.object_dir, object_dir) {
                    break;
                }
                let m = u64::from(top.num_commits());
                // git merges also when the sizes are equal (the
                // documentation says "greater than").
                let by_size = m <= u64::from(opts.size_multiple) * n;
                let by_count = opts.max_commits > 0 && n > u64::from(opts.max_commits);
                if !(by_size || by_count) {
                    break;
                }
                n += m;
                kept_layers -= 1;
            }
        }
        // Commits of merged layers join the new layer.
        let first_merged: u32 = g.layers()[..kept_layers]
            .iter()
            .map(|l| l.num_commits())
            .sum();
        for pos in first_merged..g.num_commits() {
            let id = g.id_at(pos).expect("position in range");
            if let std::collections::hash_map::Entry::Vacant(e) = parsed.entry(id) {
                e.insert(read_commit(objects, &id)?);
            }
            new.push(id);
        }
    }
    if new.is_empty() {
        return Ok(WriteReport::default());
    }
    new.sort();
    new.dedup();

    let kept_graph = base_graph.filter(|_| kept_layers > 0);
    let base_count: u32 = kept_graph.map_or(0, |g| {
        g.layers()[..kept_layers]
            .iter()
            .map(|l| l.num_commits())
            .sum()
    });
    let base_pos = |id: &ObjectId| -> Option<u32> {
        let g = kept_graph?;
        g.lookup(id).filter(|&p| p < base_count)
    };
    let write_gda = opts.generation_version >= 2
        && kept_graph.is_none_or(|g| g.layers()[kept_layers - 1].has_generation_data());

    let index: HashMap<ObjectId, usize> = new.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let entries: Vec<Entry> = new
        .iter()
        .map(|id| {
            let c = parsed.remove(id).expect("parsed");
            Entry {
                id: *id,
                tree: c.tree,
                parents: c.parents,
                date: c.date,
            }
        })
        .collect();
    let mut parent_refs: Vec<Vec<ParentRef>> = Vec::with_capacity(entries.len());
    for e in &entries {
        let mut refs = Vec::with_capacity(e.parents.len());
        for p in &e.parents {
            if let Some(&i) = index.get(p) {
                refs.push(ParentRef::New(i));
            } else if let Some(pos) = base_pos(p) {
                refs.push(ParentRef::Base(pos));
            } else {
                return Err(WriteError::UnparsableCommit(*p));
            }
        }
        parent_refs.push(refs);
    }

    // Topological levels and corrected commit dates.
    let n = entries.len();
    let levels = compute(
        n,
        &parent_refs,
        msg::COMPUTING_LEVELS,
        show,
        |i, parents: &[u64]| {
            let _ = i;
            let max = parents.iter().copied().max().unwrap_or(0);
            (max + 1).min(u64::from(GENERATION_V1_MAX))
        },
        |pos| kept_graph.map_or(0, |g| u64::from(g.level(pos))),
    );
    let corrected = compute(
        n,
        &parent_refs,
        msg::COMPUTING_GENERATIONS,
        show,
        |i, parents: &[u64]| {
            let max = parents.iter().copied().max().unwrap_or(0);
            entries[i].date.max(max + 1)
        },
        |pos| {
            kept_graph.map_or(0, |g| match g.corrected_date(pos) {
                Ok(Some(d)) => d,
                _ => u64::from(g.level(pos)),
            })
        },
    );

    // Changed-path filters.
    let mut filters: Vec<Option<Vec<u8>>> = vec![None; n];
    let mut report = WriteReport::default();
    if let Some(s) = &settings {
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by_key(|&i| corrected[i]);
        let mut computed = 0u32;
        let mut not_computed = 0u32;
        let mut trunc_empty = 0u32;
        let mut trunc_large = 0u32;
        let mut upgraded = 0u32;
        let mut p = progress(msg::COMPUTING_FILTERS, n as u64, show);
        let mut diags = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            if let Some(p) = p.as_mut() {
                p.set(k as u64 + 1);
            }
            let e = &entries[i];
            // A filter from the existing graph.
            let old = existing.and_then(|g| {
                let pos = g.lookup(&e.id)?;
                let f = g.bloom_filter(pos, &mut diags)?;
                Some((f.to_vec(), g.bloom_settings()?))
            });
            if let Some((f, os)) = &old {
                if os.same_filters(s) {
                    filters[i] = Some(f.clone());
                    not_computed += 1;
                    continue;
                }
                if os.num_hashes == s.num_hashes && os.bits_per_entry == s.bits_per_entry {
                    let parent = parent_tree(objects, e)?;
                    let paths = changed_paths(objects, hash, &e.tree, parent.as_ref());
                    if !bloom::has_high_bit(&paths) {
                        filters[i] = Some(f.clone());
                        upgraded += 1;
                        not_computed += 1;
                        continue;
                    }
                }
            }
            if opts.max_new_filters.is_some_and(|m| computed >= m) {
                not_computed += 1;
                continue;
            }
            let parent = parent_tree(objects, e)?;
            let paths = changed_paths(objects, hash, &e.tree, parent.as_ref());
            let (f, built) = bloom::build(&paths, s);
            match built {
                Built::Empty => trunc_empty += 1,
                Built::Large => trunc_large += 1,
                Built::Normal => {}
            }
            computed += 1;
            filters[i] = Some(f);
        }
        if let Some(p) = p {
            p.stop();
        }
        trace2::data_int("commit-graph", "filter-computed", i64::from(computed), true);
        trace2::data_int(
            "commit-graph",
            "filter-not-computed",
            i64::from(not_computed),
            true,
        );
        trace2::data_int(
            "commit-graph",
            "filter-trunc-empty",
            i64::from(trunc_empty),
            true,
        );
        trace2::data_int(
            "commit-graph",
            "filter-trunc-large",
            i64::from(trunc_large),
            true,
        );
        trace2::data_int("commit-graph", "filter-upgraded", i64::from(upgraded), true);
        report.filters_computed = computed;
        report.filters_not_computed = not_computed;
    }

    // Assemble the file.
    let base_hashes: Vec<ObjectId> = kept_graph.map_or_else(Vec::new, |g| {
        g.layers()[..kept_layers]
            .iter()
            .map(|l| l.checksum())
            .collect()
    });
    let content = assemble(
        hash,
        &entries,
        &parent_refs,
        base_count,
        &levels,
        write_gda.then_some(&corrected[..]),
        settings.as_ref().map(|s| (s, &filters[..])),
        &base_hashes,
        show,
    );
    let mut hasher = Hasher::new(hash);
    hasher.update(&content);
    let checksum = hasher
        .finalize()
        .map_err(|e| io::Error::other(e.to_string()))?;

    match opts.split {
        None => {
            let info = object_dir.join("info");
            std::fs::create_dir_all(&info)?;
            write_file(
                &info,
                &single_path(object_dir),
                &content,
                &checksum,
                opts.file_mode,
            )?;
            remove_chain(object_dir);
        }
        Some(_) => {
            let dir = chain_dir(object_dir);
            write_file(
                &dir,
                &layer_path(object_dir, &checksum),
                &content,
                &checksum,
                opts.file_mode,
            )?;
            let mut chain: Vec<ObjectId> = base_hashes.clone();
            chain.push(checksum);
            // The repository's single file becomes the bottom layer.
            if let Some(g) = kept_graph {
                if *g.source() == Source::Single {
                    let from = single_path(object_dir);
                    let to = layer_path(object_dir, &g.layers()[0].checksum());
                    if !to.exists() {
                        std::fs::copy(&from, &to)?;
                        set_mode(&to, opts.file_mode);
                    }
                }
            }
            let mut text = String::new();
            for h in &chain {
                text.push_str(&h.to_hex());
                text.push('\n');
            }
            write_file(
                &dir,
                &chain_path(object_dir),
                text.as_bytes(),
                &checksum,
                opts.file_mode,
            )?;
            let _ = std::fs::remove_file(single_path(object_dir));
            let old: Vec<ObjectId> = match existing.map(Graph::source) {
                Some(Source::Chain(h)) => h.clone(),
                Some(Source::Single) => existing
                    .map(|g| g.layers().iter().map(|l| l.checksum()).collect())
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            expire(object_dir, &chain, &old, opts.expire_time);
        }
    }
    report.written = n as u32;
    Ok(report)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

fn parent_tree(objects: &dyn Objects, e: &Entry) -> Result<Option<ObjectId>, WriteError> {
    match e.parents.first() {
        None => Ok(None),
        Some(p) => Ok(Some(read_commit(objects, p)?.tree)),
    }
}

/// Compute a per-commit value from the parents' values, parents first.
/// Logs the number of steps as git's `generation-dfs-steps`.
fn compute(
    n: usize,
    parents: &[Vec<ParentRef>],
    title: &str,
    show: bool,
    value: impl Fn(usize, &[u64]) -> u64,
    base: impl Fn(u32) -> u64,
) -> Vec<u64> {
    let mut out: Vec<Option<u64>> = vec![None; n];
    let mut p = progress(title, n as u64, show);
    let mut steps: i64 = 0;
    let mut stack: Vec<usize> = Vec::new();
    let mut vals: Vec<u64> = Vec::new();
    for start in 0..n {
        if let Some(p) = p.as_mut() {
            p.set(start as u64 + 1);
        }
        if out[start].is_some() {
            continue;
        }
        stack.push(start);
        while let Some(&top) = stack.last() {
            steps += 1;
            let mut ready = true;
            vals.clear();
            for r in &parents[top] {
                match *r {
                    ParentRef::New(i) => match out[i] {
                        Some(v) => vals.push(v),
                        None => {
                            ready = false;
                            stack.push(i);
                        }
                    },
                    ParentRef::Base(pos) => vals.push(base(pos)),
                }
            }
            if ready {
                out[top] = Some(value(top, &vals));
                stack.pop();
            }
        }
    }
    if let Some(p) = p {
        p.stop();
    }
    trace2::data_int("commit-graph", "generation-dfs-steps", steps, true);
    out.into_iter().map(|v| v.unwrap_or(0)).collect()
}

/// The commit date bits stored in `CDAT`.
const DATE_MASK: u64 = (1 << 34) - 1;

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    hash: HashKind,
    entries: &[Entry],
    parents: &[Vec<ParentRef>],
    base_count: u32,
    levels: &[u64],
    corrected: Option<&[u64]>,
    bloom: Option<(&bloom::Settings, &[Option<Vec<u8>>])>,
    base_hashes: &[ObjectId],
    show: bool,
) -> Vec<u8> {
    let n = entries.len();
    let pos = |r: &ParentRef| match *r {
        ParentRef::New(i) => base_count + i as u32,
        ParentRef::Base(p) => p,
    };
    let mut chunks: Vec<(u32, Vec<u8>)> = Vec::new();

    let mut fanout = Vec::with_capacity(1024);
    let mut count = 0usize;
    for b in 0..256usize {
        while count < n && usize::from(entries[count].id.as_bytes()[0]) <= b {
            count += 1;
        }
        put32(&mut fanout, count as u32);
    }
    chunks.push((id::OIDF, fanout));

    let mut lookup = Vec::with_capacity(n * hash.byte_len());
    for e in entries {
        lookup.extend_from_slice(e.id.as_bytes());
    }
    chunks.push((id::OIDL, lookup));

    let mut cdat = Vec::with_capacity(n * (hash.byte_len() + 16));
    let mut edges: Vec<u8> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        cdat.extend_from_slice(e.tree.as_bytes());
        let ps = &parents[i];
        let p1 = ps.first().map_or(NO_PARENT, pos);
        let p2 = match ps.len() {
            0 | 1 => NO_PARENT,
            2 => pos(&ps[1]),
            _ => {
                let at = (edges.len() / 4) as u32;
                for (k, r) in ps[1..].iter().enumerate() {
                    let mut v = pos(r);
                    if k + 2 == ps.len() {
                        v |= 0x8000_0000;
                    }
                    put32(&mut edges, v);
                }
                0x8000_0000 | at
            }
        };
        put32(&mut cdat, p1);
        put32(&mut cdat, p2);
        let level = levels[i] as u32;
        put32(&mut cdat, (level << 2) | ((e.date >> 32) as u32 & 3));
        put32(&mut cdat, e.date as u32);
    }
    chunks.push((id::CDAT, cdat));

    if let Some(corrected) = corrected {
        let mut gda = Vec::with_capacity(n * 4);
        let mut overflow = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            // The offset is relative to the stored (34-bit) date.
            let offset = corrected[i].wrapping_sub(e.date & DATE_MASK);
            if offset > 0x7fff_ffff {
                put32(&mut gda, 0x8000_0000 | (overflow.len() / 8) as u32);
                overflow.extend_from_slice(&offset.to_be_bytes());
            } else {
                put32(&mut gda, offset as u32);
            }
        }
        chunks.push((id::GDA2, gda));
        if !overflow.is_empty() {
            chunks.push((id::GDO2, overflow));
        }
    }
    if !edges.is_empty() {
        chunks.push((id::EDGE, edges));
    }
    if let Some((s, filters)) = bloom {
        let mut index = Vec::with_capacity(n * 4);
        let mut data = Vec::new();
        put32(&mut data, s.hash_version);
        put32(&mut data, s.num_hashes);
        put32(&mut data, s.bits_per_entry);
        let mut total = 0u32;
        for f in filters {
            if let Some(f) = f {
                data.extend_from_slice(f);
                total += f.len() as u32;
            }
            put32(&mut index, total);
        }
        chunks.push((id::BIDX, index));
        chunks.push((id::BDAT, data));
    }
    if !base_hashes.is_empty() {
        let mut base = Vec::new();
        for h in base_hashes {
            base.extend_from_slice(h.as_bytes());
        }
        chunks.push((id::BASE, base));
    }

    let passes = chunks.len();
    let mut p = progress(&msg::writing_passes(passes), (passes * n) as u64, show);
    let _region = trace2::region("chunkfile", "write", true);
    if let Some((s, _)) = bloom {
        trace2::data_json("bloom", "settings", &s.json(), true);
    }
    let mut out = Vec::new();
    put32(&mut out, SIGNATURE);
    out.push(1);
    out.push(hash_version(hash));
    out.push(chunks.len() as u8);
    out.push(base_hashes.len() as u8);
    let mut offset = (8 + (chunks.len() + 1) * 12) as u64;
    for (cid, data) in &chunks {
        put32(&mut out, *cid);
        out.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    put32(&mut out, 0);
    out.extend_from_slice(&offset.to_be_bytes());
    let mut done = 0u64;
    for (_, data) in &chunks {
        out.extend_from_slice(data);
        done += n as u64;
        if let Some(p) = p.as_mut() {
            p.set(done);
        }
    }
    if let Some(p) = p {
        p.stop();
    }
    out
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// Write `content` (plus `checksum` unless it is the chain file) to a
/// temporary file in `dir` and rename it to `dest`.
fn write_file(
    dir: &Path,
    dest: &Path,
    content: &[u8],
    checksum: &ObjectId,
    mode: u32,
) -> io::Result<()> {
    let is_chain = dest.file_name().is_some_and(|n| n == "commit-graph-chain");
    let mut tmp = tempfile_in(dir)?;
    tmp.1.write_all(content)?;
    if !is_chain {
        tmp.1.write_all(checksum.as_bytes())?;
    }
    tmp.1.sync_all().ok();
    drop(tmp.1);
    set_mode(&tmp.0, mode);
    if let Err(e) = std::fs::rename(&tmp.0, dest) {
        let _ = std::fs::remove_file(&tmp.0);
        return Err(e);
    }
    Ok(())
}

fn tempfile_in(dir: &Path) -> io::Result<(PathBuf, std::fs::File)> {
    let pid = std::process::id();
    for i in 0..1000u32 {
        let p = dir.join(format!("tmp_graph_{pid}_{i}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(f) => return Ok((p, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("cannot create temporary file"))
}

/// After writing a single file: the chain and its layers are gone.
fn remove_chain(object_dir: &Path) {
    let dir = chain_dir(object_dir);
    let _ = std::fs::remove_file(chain_path(object_dir));
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("graph-") && name.ends_with(".graph") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Remove `*.graph` files of the chain directory that `chain` does not
/// name and that were last modified at `expire` or earlier. Files that
/// just became unused count as modified now.
fn expire(object_dir: &Path, chain: &[ObjectId], old: &[ObjectId], expire: Option<i64>) {
    let dir = chain_dir(object_dir);
    let keep: HashSet<String> = chain
        .iter()
        .map(|h| format!("graph-{}.graph", h.to_hex()))
        .collect();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let limit = expire.unwrap_or(now);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".graph") || keep.contains(&name) {
            continue;
        }
        let path = e.path();
        // Unused layers of the chain just replaced count as modified now.
        let was_layer = old
            .iter()
            .any(|h| name == format!("graph-{}.graph", h.to_hex()));
        let mtime = if was_layer {
            touch(&path);
            now
        } else {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64)
        };
        if mtime <= limit {
            let _ = std::fs::remove_file(&path);
        }
    }
}

fn touch(path: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    } else {
        // Read-only file: make it writable just for the time stamp.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(path) {
                let mode = m.permissions().mode();
                set_mode(path, mode | 0o200);
                if let Ok(f) = std::fs::OpenOptions::new().append(true).open(path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                set_mode(path, mode);
            }
        }
    }
}
