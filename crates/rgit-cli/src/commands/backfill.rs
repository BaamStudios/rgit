// SPDX-License-Identifier: MIT
//! `git backfill` (Documentation/git-backfill.adoc): download the missing
//! blobs of a partial clone in batches.
//!
//! The commits the revision arguments select (`HEAD` by default) and, with
//! `--include-edges` (the default), their excluded parents give root trees.
//! These are walked path by path: all trees found at one path are read
//! together, and the blobs found at one path are requested together. A
//! batch is sent to the promisor remotes once it holds at least
//! `--min-batch-size` blobs (only after a whole path was added), the rest
//! at the end. `--sparse` (on when `core.sparseCheckout` is) keeps the
//! paths of the sparse checkout; a pathspec only narrows the blobs that
//! are requested.
//!
//! Trace2 reports the walk like git: region `path-walk/commit-walk` with
//! data `commits`, region `path-walk/path-walk` with data `paths` (the
//! number of tree and blob paths walked).

use std::collections::{BTreeMap, HashSet};

use rgit_hash::ObjectId;
use rgit_object::{Object, ObjectKind, TreeIter};
use rgit_pack::write::ObjectFilter;
use rgit_pathspec::Pathspec;
use rgit_worktree::sparse::{DirMatch, Sparse};

use crate::messages::backfill as msg;
use crate::prelude::*;
use crate::repository::Repo;

const DEFAULT_BATCH: u64 = 50_000;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let min_batch = Cell::new(DEFAULT_BATCH);
    let sparse = Cell::new(-1i32);
    let edges = Cell::new(true);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::unsigned(None, "min-batch-size", &min_batch, msg::H_MIN_BATCH_SIZE),
            Opt::bool(None, "sparse", &sparse, msg::H_SPARSE),
            Opt::flag(None, "include-edges", &edges, msg::H_INCLUDE_EDGES),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        keep_unknown_opt: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    drop(parser);

    // The object options of `rev-list` are taken here; the rest goes to
    // the revision walk.
    let dashdash = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let mut rev_args = Vec::new();
    let mut objects = false;
    let mut filters: Vec<String> = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if i < dashdash {
            if a == "--objects" {
                objects = true;
                continue;
            }
            if let Some(spec) = a.strip_prefix("--filter=") {
                filters.push(spec.to_owned());
                continue;
            }
            if a == "--no-filter" {
                filters.clear();
                continue;
            }
            if a.starts_with('-') && a != "-" && !crate::history::is_rev_list_option(a) {
                return Err(Die::Fatal(msg::unrecognized_argument(a)));
            }
        }
        rev_args.push(a.clone());
    }
    let filter = crate::promisor::parse_filters(&filters)?.map(|(f, _)| f);
    if filter.is_some() && !objects {
        return Err(Die::Fatal(
            crate::messages::rev_list::FILTER_NEEDS_OBJECTS.into(),
        ));
    }
    if let Some(f) = &filter {
        check_filter(f)?;
    }

    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    let use_sparse = match sparse.get() {
        -1 => crate::sparse::enabled(&repo.config),
        v => v != 0,
    };
    let sparse = if use_sparse {
        match crate::sparse::load(&repo) {
            Some(s) => Some(s),
            None => return Err(Die::Error(msg::PROBLEM_LOADING_SPARSE.into(), 255)),
        }
    } else {
        None
    };
    let mut keep = Keep {
        sparse,
        sparse_oid: None,
        pathspec: Pathspec::default(),
        blobs: filter.as_ref().is_none_or(wants_blobs),
    };
    if let Some(spec) = filter.as_ref().and_then(sparse_oid) {
        let data = repo
            .resolve(spec)
            .and_then(|id| match repo.read_object(&id) {
                Some(Object::Blob(b)) => Some(b.data),
                _ => None,
            })
            .ok_or_else(|| Die::Error(msg::no_sparse_blob(spec), 255))?;
        keep.sparse_oid = Some(Sparse::parse(&data, false, false).0);
    }

    let walk = crate::history::walk_backfill(ctx, &rev_args, edges.get())?;
    if !walk.paths.is_empty() {
        keep.pathspec = pathspec(ctx, &repo, &walk.paths)?;
    }

    let mut roots = Vec::new();
    {
        let _region = rgit_trace::trace2::region("path-walk", "commit-walk", true);
        for c in walk.commits.iter().chain(&walk.edges) {
            if let Some(t) = repo.peel_to(*c, ObjectKind::Tree) {
                roots.push(t);
            }
        }
        rgit_trace::trace2::data(
            "path-walk",
            "commits",
            &walk.commits.len().to_string(),
            true,
        );
    }
    let mut batch = Batch {
        min: min_batch.get(),
        ids: Vec::new(),
    };
    {
        let _region = rgit_trace::trace2::region("path-walk", "path-walk", true);
        let paths = walk_paths(&repo, &keep, roots, &mut batch);
        rgit_trace::trace2::data("path-walk", "paths", &paths.to_string(), true);
    }
    batch.flush(&repo);
    Ok(0)
}

/// `--filter` specs `backfill` cannot work with.
fn check_filter(f: &ObjectFilter) -> Result<(), Die> {
    match f {
        ObjectFilter::TreeDepth(_) => {
            eprintln!("error: {}", msg::unsupported_filter(&f.to_spec()));
            Err(Die::Fatal(msg::BAD_FILTER.into()))
        }
        ObjectFilter::BlobLimit(_) => Err(Die::Fatal(msg::BLOB_LIMIT.into())),
        ObjectFilter::Combine(parts) => parts.iter().try_for_each(check_filter),
        _ => Ok(()),
    }
}

/// Whether blobs pass the filter at all.
fn wants_blobs(f: &ObjectFilter) -> bool {
    match f {
        ObjectFilter::BlobNone => false,
        ObjectFilter::ObjectType(k) => *k == ObjectKind::Blob,
        ObjectFilter::Combine(parts) => parts.iter().all(wants_blobs),
        _ => true,
    }
}

/// The blob of a `sparse:oid=` filter.
fn sparse_oid(f: &ObjectFilter) -> Option<&str> {
    match f {
        ObjectFilter::SparseOid(s) => Some(s),
        ObjectFilter::Combine(parts) => parts.iter().find_map(sparse_oid),
        _ => None,
    }
}

fn pathspec(ctx: &Context, repo: &Repo, args: &[String]) -> Result<Pathspec, Die> {
    let mut prefix = repo.prefix();
    if !prefix.is_empty() && !prefix.ends_with(b"/") {
        prefix.push(b'/');
    }
    let opts = rgit_pathspec::ParseOptions {
        prefix,
        root: repo
            .setup
            .work_tree
            .as_ref()
            .map(|w| w.as_os_str().as_encoded_bytes().to_vec()),
        globals: crate::repository::pathspec_globals(ctx),
        ..Default::default()
    };
    Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))
}

/// Which paths are walked.
struct Keep {
    sparse: Option<Sparse>,
    sparse_oid: Option<Sparse>,
    pathspec: Pathspec,
    /// The filter lets blobs through.
    blobs: bool,
}

impl Keep {
    /// Whether the directory `dir` is walked.
    fn dir(&self, dir: &[u8]) -> bool {
        self.sparse
            .as_ref()
            .is_none_or(|s| s.dir_match(dir) != Some(DirMatch::Excluded))
    }

    /// Whether the blobs at `path` are walked.
    fn file(&self, path: &[u8]) -> bool {
        self.sparse.as_ref().is_none_or(|s| s.includes(path))
            && self.sparse_oid.as_ref().is_none_or(|s| s.includes(path))
    }

    /// Whether the blobs at `path` are fetched (a pathspec narrows what
    /// is fetched, not what is walked).
    fn fetch(&self, path: &[u8]) -> bool {
        self.blobs && (self.pathspec.is_empty() || self.pathspec.matches(path))
    }
}

/// Missing blobs waiting to be requested.
struct Batch {
    min: u64,
    ids: Vec<ObjectId>,
}

impl Batch {
    /// Add the missing blobs of one path; send the batch once it is big
    /// enough.
    fn add(&mut self, repo: &Repo, ids: Vec<ObjectId>) {
        self.ids.extend(ids);
        if !self.ids.is_empty() && self.ids.len() as u64 >= self.min {
            self.flush(repo);
        }
    }

    fn flush(&mut self, repo: &Repo) {
        if !self.ids.is_empty() {
            repo.odb().prefetch(&std::mem::take(&mut self.ids));
        }
    }
}

/// Walk the trees `roots` path by path and request the missing blobs.
/// Returns the number of paths walked.
fn walk_paths(repo: &Repo, keep: &Keep, roots: Vec<ObjectId>, batch: &mut Batch) -> usize {
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut count = 0;
    // Directories still to walk (without trailing slash, "" is the top),
    // each with the trees found there.
    let mut stack: Vec<(Vec<u8>, Vec<ObjectId>)> = Vec::new();
    let roots: Vec<ObjectId> = roots.into_iter().filter(|t| seen.insert(*t)).collect();
    if !roots.is_empty() {
        stack.push((Vec::new(), roots));
    }
    while let Some((dir, trees)) = stack.pop() {
        count += 1;
        let mut subdirs: BTreeMap<Vec<u8>, Vec<ObjectId>> = BTreeMap::new();
        let mut files: BTreeMap<Vec<u8>, Vec<ObjectId>> = BTreeMap::new();
        for t in &trees {
            let Some(data) = repo.read_tree_data(t) else {
                continue;
            };
            for e in TreeIter::new(repo.hash, &data).filter_map(Result::ok) {
                let mut path = dir.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(e.name);
                if e.is_tree() {
                    subdirs.entry(path).or_default().push(e.oid);
                } else if e.mode_bits & 0o170000 != 0o160000 {
                    files.entry(path).or_default().push(e.oid);
                }
            }
        }
        for (path, ids) in files {
            if !keep.file(&path) {
                continue;
            }
            count += 1;
            let ids: Vec<ObjectId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
            if !keep.fetch(&path) {
                continue;
            }
            let missing: Vec<ObjectId> = ids
                .into_iter()
                .filter(|id| !repo.odb().contains(id))
                .collect();
            batch.add(repo, missing);
        }
        for (path, ids) in subdirs.into_iter().rev() {
            if !keep.dir(&path) {
                continue;
            }
            let ids: Vec<ObjectId> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
            if !ids.is_empty() {
                stack.push((path, ids));
            }
        }
    }
    count
}
