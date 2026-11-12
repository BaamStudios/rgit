// SPDX-License-Identifier: MIT
//! `git read-tree` (Documentation/git-read-tree.adoc), first slice:
//! `read-tree <tree-ish>` (replaces the index and primes the cache tree)
//! and `read-tree --empty`. Merges (`-m`), `-u`, `--prefix` and `--reset`
//! are not implemented yet (#107).

use rgit_hash::ObjectId;
use rgit_index::{
    AddOptions, CacheTree, Entry, Index, Mode as IndexMode, Stage, TreeNode, WriteOptions,
};
use rgit_object::{Object, ObjectKind};

use crate::messages::read_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

/// Appends the entries of `tree` below `base` to `out` and returns the
/// cache-tree node for it.
fn walk(
    repo: &Repo,
    tree: &ObjectId,
    name: &[u8],
    base: &[u8],
    out: &mut Vec<Entry>,
) -> Result<TreeNode, Die> {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(msg::failed_to_unpack(&tree.to_string())));
    };
    let start = out.len();
    let mut children = Vec::new();
    for e in t.entries {
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical();
        if mode.is_tree() {
            path.push(b'/');
            children.push(walk(repo, &e.oid, &e.name, &path, out)?);
        } else {
            out.push(Entry::new(path, Stage::MERGED, IndexMode(mode.bits()), e.oid));
        }
    }
    let count = (out.len() - start) as u32;
    let mut node = TreeNode::new_valid(name.to_vec(), count, *tree);
    children.sort_by(|a, b| a.name.len().cmp(&b.name.len()).then(a.name.cmp(&b.name)));
    node.children = children;
    Ok(node)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let empty = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "empty", &empty, msg::HELP_EMPTY),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.len() > 1 || (empty.get() && !args.is_empty()) {
        return Err(parser.usage_error().into());
    }
    if args.is_empty() && !empty.get() {
        eprintln!("warning: {}", msg::EMPTY_DEPRECATED);
    }
    drop(parser);

    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let tree = match args.first() {
        Some(spec) => Some(
            repo.resolve(spec)
                .and_then(|id| repo.peel_to(id, ObjectKind::Tree))
                .ok_or_else(|| Die::Fatal(msg::not_valid_object_name(spec)))?,
        ),
        None => None,
    };
    let repo = repo;
    let (old, _) = repo.read_index()?;
    let mut index = Index::new(repo.hash);
    if old.version() != 2 {
        index.set_version(old.version());
    }
    if let Some(tree) = tree {
        let mut entries = Vec::new();
        let root = walk(&repo, &tree, b"", b"", &mut entries)?;
        for e in entries {
            index
                .add(e, AddOptions::default())
                .map_err(|err| Die::Fatal(err.to_string()))?;
        }
        index.set_cache_tree(Some(CacheTree { root }));
    } else {
        let root = TreeNode::new_valid(Vec::new(), 0, ObjectId::empty_tree(repo.hash));
        index.set_cache_tree(Some(CacheTree { root }));
    }

    let path = repo.setup.cwd.join(repo.index_path());
    let skip_hash = repo
        .config
        .bool("index.skiphash")
        .ok()
        .flatten()
        .unwrap_or(false);
    match index.write_file(
        &path,
        &rgit_fs::LockOptions::default(),
        &WriteOptions { skip_hash },
    ) {
        Ok(()) => Ok(0),
        Err(rgit_index::WriteError::Lock(e)) => {
            if let Some(w) = e.warning() {
                eprintln!("warning: {w}");
            }
            Err(Die::Fatal(e.to_string()))
        }
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}
