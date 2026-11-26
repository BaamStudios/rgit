// SPDX-License-Identifier: MIT
//! `git last-modified` (Documentation/git-last-modified.adoc): the commit
//! that last changed each path of a tree.
//!
//! The paths are the entries of the starting commit's tree that the
//! pathspec and `--max-depth` select (as `diff-tree --max-depth` would show
//! them against an empty tree). Walking the history newest first, a commit
//! passes a path on to the first parent that has the same entry (mode and
//! object); a commit none of whose parents has it changed the path. A path
//! handed to a parent outside the walked range is printed with a `^` before
//! that parent's id, after the paths the commit changed, sorted by path.
//!
//! Paths found at the same commit are printed in the order of a hash table
//! keyed by path: 32-bit FNV-1 hashes, 64 buckets growing fourfold above
//! 80% load, newest entry first in a bucket (determined black-box against
//! git 2.56.1).

use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;

use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, TreeIter};
use rgit_pathspec::{GlobalFlags, Magic, ParseOptions, Pathspec};

use crate::diff::collect::TreeDiff;
use crate::messages::last_modified as msg;
use crate::prelude::*;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let recursive = Cell::new(false);
    let show_trees = Cell::new(false);
    let max_depth: Cell<i32> = Cell::new(0);
    let nul = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('r', "recursive", &recursive, msg::H_RECURSIVE),
            Opt::flag('t', "show-trees", &show_trees, msg::H_SHOW_TREES),
            Opt::integer(None, "max-depth", &max_depth, msg::H_MAX_DEPTH)
                .argh("n")
                .noneg(),
            Opt::flag('z', None, &nul, msg::H_Z).noneg(),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        keep_unknown_opt: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    // Options the revision walk does not know either.
    let dashdash = args.iter().position(|a| a == "--").unwrap_or(args.len());
    for a in &args[..dashdash] {
        if a.starts_with('-')
            && a != "-"
            && !crate::history::is_rev_list_option(a)
        {
            eprintln!("error: {}", msg::unknown_argument(a));
            return Err(parser.usage_error().into());
        }
    }
    drop(parser);

    let mut repo = Repo::open(ctx, crate::repo::Mode::Required)?;
    let prefix = repo
        .setup
        .prefix
        .as_ref()
        .map(|p| p.as_encoded_bytes().to_vec())
        .unwrap_or_default();

    // Split revisions from paths.
    let mut rev_args: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut positive: Vec<(String, ObjectId)> = Vec::new();
    let mut seen_path = false;
    for (i, a) in args.iter().enumerate() {
        if i == dashdash {
            continue;
        }
        if i > dashdash || seen_path {
            paths.push(a.clone());
            continue;
        }
        if a.starts_with('-') {
            rev_args.push(a.clone());
            continue;
        }
        match classify(&mut repo, a) {
            Some(Rev::Positive(id)) => {
                positive.push((a.clone(), id));
                rev_args.push(a.clone());
            }
            Some(Rev::Other) => rev_args.push(a.clone()),
            None if dashdash < args.len() => {
                return Err(Die::Fatal(crate::messages::rev_list::bad_revision(a)));
            }
            None => {
                if !looks_like_path(&repo, &prefix, a) {
                    return Err(Die::Fatal(crate::messages::rev_list::ambiguous_argument(
                        a,
                    )));
                }
                seen_path = true;
                paths.push(a.clone());
            }
        }
    }
    if positive.len() > 1 {
        return Err(Die::Error(msg::ONE_COMMIT.into(), 255));
    }
    let start = match positive.first() {
        Some((name, id)) => match repo.peel_to(*id, ObjectKind::Commit) {
            Some(c) => c,
            None => {
                let kind = repo
                    .peel_to(*id, ObjectKind::Tree)
                    .map_or("blob", |_| "tree");
                let kind = match repo.object_kind(id) {
                    Some(ObjectKind::Tree) => "tree",
                    Some(ObjectKind::Blob) => "blob",
                    _ => kind,
                };
                return Err(Die::Error(msg::not_commitish(name, kind), 255));
            }
        },
        None => {
            rev_args.push("HEAD".into());
            match repo.resolve("HEAD") {
                Some(id) => repo
                    .peel_to(id, ObjectKind::Commit)
                    .ok_or_else(|| Die::Fatal(crate::messages::rev_list::bad_revision("HEAD")))?,
                None => return Err(Die::Fatal(crate::messages::rev_list::bad_revision("HEAD"))),
            }
        }
    };

    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let ps_opts = ParseOptions {
        prefix: prefix.clone(),
        root,
        globals: GlobalFlags::from_env().map_err(|e| Die::Fatal(e.to_string()))?,
        allowed_magic: Magic::ALL,
        ..Default::default()
    };
    let pathspec = Pathspec::parse(&paths, &ps_opts).map_err(|e| Die::Fatal(e.to_string()))?;

    let commit_tree = |repo: &Repo, id: &ObjectId| -> Option<ObjectId> {
        let data = repo.odb.read(id).ok().flatten()?;
        rgit_object::Commit::parse(repo.hash, &data.data).ok()?.tree().ok()
    };
    let tree = commit_tree(&repo, &start)
        .ok_or_else(|| Die::Fatal(crate::messages::rev_list::bad_revision("HEAD")))?;

    // The paths of interest.
    let depth = if recursive.get() {
        None
    } else {
        usize::try_from(max_depth.get()).ok()
    };
    let mut diff = TreeDiff {
        repo: &repo,
        pathspec: &pathspec,
        recursive: true,
        show_trees: show_trees.get(),
        max_depth: depth,
        pairs: Vec::new(),
    };
    diff.walk(None, Some(&tree), b"")?;
    let targets: Vec<(Vec<u8>, u32, ObjectId)> = diff
        .pairs
        .into_iter()
        .filter_map(|p| p.new.map(|s| (p.path, s.mode, s.oid)))
        .collect();
    let order = HashOrder::new(targets.iter().map(|t| t.0.as_slice()));

    let walked = crate::history::walk_with_parents(ctx, &rev_args)?;
    let in_walk: HashMap<ObjectId, usize> = walked
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, i))
        .collect();

    let mut lookup = TreeLookup::new(&repo);
    let mut active: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    let mut out = Vec::new();
    let mut boundary: Vec<(usize, ObjectId)> = Vec::new();
    let write = |out: &mut Vec<u8>, id: &ObjectId, path: &[u8], mark: bool| {
        if mark {
            out.push(b'^');
        }
        out.extend_from_slice(id.to_string().as_bytes());
        out.push(b'\t');
        if nul.get() {
            out.extend_from_slice(path);
            out.push(0);
        } else {
            crate::quote::write_quoted(out, path, true);
            out.push(b'\n');
        }
    };
    // Paths handed to commits outside the walk, sorted by path.
    let flush_boundary = |out: &mut Vec<u8>, boundary: &mut Vec<(usize, ObjectId)>| {
        boundary.sort_by(|a, b| targets[a.0].0.cmp(&targets[b.0].0));
        for (i, id) in boundary.drain(..) {
            write(out, &id, &targets[i].0, true);
        }
    };
    if in_walk.contains_key(&start) {
        active.insert(start, (0..targets.len()).collect());
    } else {
        boundary.extend((0..targets.len()).map(|i| (i, start)));
    }
    for (id, parents) in &walked {
        let Some(paths) = active.remove(id) else {
            continue;
        };
        let mut found: Vec<usize> = Vec::new();
        let ctree = commit_tree(&repo, id);
        let mut parent_trees: Vec<Option<ObjectId>> = Vec::new();
        for p in parents {
            parent_trees.push(commit_tree(&repo, p));
        }
        for idx in paths {
            let path = &targets[idx].0;
            let mine = ctree.and_then(|t| lookup.entry(&t, path));
            let mut passed = None;
            if mine.is_some() {
                for (p, pt) in parents.iter().zip(&parent_trees) {
                    if pt.and_then(|t| lookup.entry(&t, path)) == mine {
                        passed = Some(*p);
                        break;
                    }
                }
            }
            match passed {
                Some(p) if in_walk.contains_key(&p) => active.entry(p).or_default().push(idx),
                Some(p) => boundary.push((idx, p)),
                None => found.push(idx),
            }
        }
        found.sort_by_key(|&i| order.position(i));
        for i in found {
            write(&mut out, id, &targets[i].0, false);
        }
        flush_boundary(&mut out, &mut boundary);
    }
    flush_boundary(&mut out, &mut boundary);
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(&out);
    let _ = stdout.flush();
    Ok(0)
}

enum Rev {
    /// A commit-ish (or other object) to start from.
    Positive(ObjectId),
    /// An excluded commit or a range.
    Other,
}

fn classify(repo: &mut Repo, arg: &str) -> Option<Rev> {
    if let Some(rest) = arg.strip_prefix('^') {
        return repo.resolve(rest).map(|_| Rev::Other);
    }
    let side = |repo: &mut Repo, s: &str| repo.resolve(if s.is_empty() { "HEAD" } else { s });
    if let Some((a, b)) = arg.split_once("...") {
        let left = side(repo, a)?;
        side(repo, b)?;
        return Some(Rev::Positive(left));
    }
    if let Some((a, b)) = arg.split_once("..") {
        side(repo, a)?;
        return side(repo, b).map(Rev::Positive);
    }
    repo.resolve(arg).map(Rev::Positive)
}

/// An argument that is not a revision names a path when it exists in the
/// work tree or is a wildcard pattern.
fn looks_like_path(repo: &Repo, prefix: &[u8], arg: &str) -> bool {
    if rgit_pathspec::wildmatch::has_glob_special(arg.as_bytes()) || arg.starts_with(':') {
        return true;
    }
    let Some(top) = repo.setup.work_tree.as_ref() else {
        return false;
    };
    let mut p = top.join(std::path::Path::new(
        std::str::from_utf8(prefix).unwrap_or_default(),
    ));
    p.push(arg);
    std::fs::symlink_metadata(p).is_ok()
}

/// Entries of trees by path, with the trees read once.
struct TreeLookup<'r> {
    repo: &'r Repo,
    trees: HashMap<ObjectId, Rc<[u8]>>,
}

impl<'r> TreeLookup<'r> {
    fn new(repo: &'r Repo) -> Self {
        TreeLookup {
            repo,
            trees: HashMap::new(),
        }
    }

    /// Mode and id of `path` in `tree`.
    fn entry(&mut self, tree: &ObjectId, path: &[u8]) -> Option<(u32, ObjectId)> {
        let mut cur = *tree;
        let mut found = None;
        for comp in path.split(|&b| b == b'/') {
            if let Some((mode, _)) = found
                && mode & 0o170000 != 0o040000
            {
                return None;
            }
            let data = match self.trees.get(&cur) {
                Some(d) => d.clone(),
                None => {
                    let d = self.repo.read_tree_data(&cur)?;
                    self.trees.insert(cur, d.clone());
                    d
                }
            };
            let e = TreeIter::new(self.repo.hash, &data)
                .filter_map(Result::ok)
                .find(|e| e.name == comp)?;
            found = Some((e.mode_bits, e.oid));
            cur = e.oid;
        }
        found
    }
}

/// The iteration order of a hash table keyed by path (see the module
/// documentation).
struct HashOrder {
    positions: Vec<(usize, usize)>,
}

impl HashOrder {
    fn new<'a>(paths: impl Iterator<Item = &'a [u8]>) -> Self {
        let hashes: Vec<u32> = paths.map(fnv1).collect();
        let mut size = 64usize;
        // Each bucket lists its entries newest first.
        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); size];
        for (i, &h) in hashes.iter().enumerate() {
            buckets[h as usize & (size - 1)].insert(0, i);
            if i + 1 > size * 80 / 100 {
                size <<= 2;
                let mut grown: Vec<Vec<usize>> = vec![Vec::new(); size];
                for b in &buckets {
                    for &e in b {
                        grown[hashes[e] as usize & (size - 1)].insert(0, e);
                    }
                }
                buckets = grown;
            }
        }
        let mut positions = vec![(0, 0); hashes.len()];
        for (b, bucket) in buckets.iter().enumerate() {
            for (k, &e) in bucket.iter().enumerate() {
                positions[e] = (b, k);
            }
        }
        HashOrder { positions }
    }

    fn position(&self, i: usize) -> (usize, usize) {
        self.positions[i]
    }
}

/// 32-bit FNV-1.
fn fnv1(s: &[u8]) -> u32 {
    s.iter().fold(0x811c_9dc5u32, |h, &c| {
        h.wrapping_mul(0x0100_0193) ^ u32::from(c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_within_buckets_is_newest_first() {
        let paths: Vec<&[u8]> = vec![b"c/d", b"z"];
        let o = HashOrder::new(paths.into_iter());
        assert_eq!(o.position(0).0, o.position(1).0);
        assert!(o.position(1) < o.position(0));
    }
}
