// SPDX-License-Identifier: MIT
//! `git write-tree` (Documentation/git-write-tree.adoc): create a tree
//! object from the index.

use rgit_hash::ObjectId;
use rgit_index::Index;
use rgit_object::{EntryMode, ObjectKind, Tree, TreeEntry};

use crate::messages::write_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

type Item<'a> = (&'a [u8], u32, ObjectId);

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let missing_ok = Cell::new(false);
    let prefix = RefCell::new(None);
    let mut parser = Parser::new(
        msg::WRITE_TREE_USAGE,
        vec![
            Opt::flag(None, "missing-ok", &missing_ok, msg::HELP_MISSING_OK),
            Opt::string(None, "prefix", &prefix, "<prefix>/", msg::HELP_PREFIX),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let (index, _) = repo.read_index()?;
    let prefix = prefix.into_inner();
    let oid = write_tree(&repo, &index, missing_ok.get(), prefix.as_deref())?;
    println!("{oid}");
    Ok(0)
}

/// Write the trees for `index` (or its `prefix` subdirectory) to the
/// object database and return the top tree's id.
pub fn write_tree(
    repo: &Repo,
    index: &Index,
    missing_ok: bool,
    prefix: Option<&str>,
) -> Result<ObjectId, Die> {
    let mut items: Vec<Item<'_>> = Vec::new();
    let mut unmerged = false;
    let mut last_unmerged: Option<&[u8]> = None;
    for e in index.entries() {
        if e.stage().get() != 0 {
            if last_unmerged != Some(e.path()) {
                eprintln!("{}", msg::unmerged(&String::from_utf8_lossy(e.path())));
            }
            last_unmerged = Some(e.path());
            unmerged = true;
            continue;
        }
        if e.flags.intent_to_add {
            continue;
        }
        items.push((e.path(), e.mode.0, e.id));
    }
    if unmerged {
        return Err(Die::Fatal(msg::ERROR_BUILDING_TREES.into()));
    }
    if !missing_ok {
        for (path, mode, id) in &items {
            if *mode & 0o170000 == 0o160000 {
                continue;
            }
            if !matches!(repo.odb().header(id), Ok(Some(_))) {
                eprintln!(
                    "error: {}",
                    msg::invalid_object(*mode, &id.to_hex(), &String::from_utf8_lossy(path))
                );
                return Err(Die::Fatal(msg::ERROR_BUILDING_TREES.into()));
            }
        }
    }
    if let Some(p) = prefix {
        let dir = p.trim_end_matches('/');
        if !dir.is_empty() {
            let want = format!("{dir}/");
            let sub: Vec<Item<'_>> = items
                .iter()
                .filter(|it| it.0.starts_with(want.as_bytes()))
                .copied()
                .collect();
            if sub.is_empty() {
                return Err(Die::Fatal(msg::prefix_not_found(p)));
            }
            return build(repo, &sub, want.len());
        }
    }
    build(repo, &items, 0)
}

/// Build the tree for `items` (in index order), whose paths all share the
/// first `strip` bytes.
fn build(repo: &Repo, items: &[Item<'_>], strip: usize) -> Result<ObjectId, Die> {
    let bad_mode = || Die::Fatal(msg::ERROR_BUILDING_TREES.into());
    let mut tree = Tree::new();
    let mut i = 0;
    while i < items.len() {
        let rest = &items[i].0[strip..];
        match rest.iter().position(|&c| c == b'/') {
            None => {
                let mode = EntryMode::from_octal(format!("{:o}", items[i].1).as_bytes())
                    .ok_or_else(bad_mode)?;
                tree.entries.push(TreeEntry::new(mode, rest, items[i].2));
                i += 1;
            }
            Some(slash) => {
                let name = &rest[..=slash];
                let mut j = i + 1;
                while j < items.len() && items[j].0[strip..].starts_with(name) {
                    j += 1;
                }
                let oid = build(repo, &items[i..j], strip + slash + 1)?;
                let mode = EntryMode::from_octal(b"40000").ok_or_else(bad_mode)?;
                tree.entries
                    .push(TreeEntry::new(mode, &rest[..slash], oid));
                i = j;
            }
        }
    }
    tree.entries.sort_by(TreeEntry::cmp_tree_order);
    repo.odb()
        .write(ObjectKind::Tree, &tree.serialize())
        .map_err(|e| Die::Fatal(e.to_string()))
}
