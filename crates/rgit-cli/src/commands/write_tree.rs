// SPDX-License-Identifier: MIT
//! `git write-tree` (Documentation/git-write-tree.adoc): create a tree
//! object from the index. The tree building lives in `rgit-commit`.

use rgit_commit::{WriteTreeError, WriteTreeOptions};
use rgit_hash::ObjectId;
use rgit_index::Index;

use crate::messages::write_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

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
/// object database and return the top tree's id; problems are reported
/// the way `git write-tree` reports them.
pub fn write_tree(
    repo: &Repo,
    index: &Index,
    missing_ok: bool,
    prefix: Option<&str>,
) -> Result<ObjectId, Die> {
    let opts = WriteTreeOptions { missing_ok, prefix };
    rgit_commit::write_tree(repo.odb(), index, &opts).map_err(|e| match e {
        WriteTreeError::Unmerged(paths) => {
            for p in paths {
                eprintln!("{}", msg::unmerged(&String::from_utf8_lossy(&p)));
            }
            Die::Fatal(msg::ERROR_BUILDING_TREES.into())
        }
        WriteTreeError::MissingObject { mode, id, path } => {
            eprintln!(
                "error: {}",
                msg::invalid_object(mode, &id.to_hex(), &String::from_utf8_lossy(&path))
            );
            Die::Fatal(msg::ERROR_BUILDING_TREES.into())
        }
        WriteTreeError::PrefixNotFound(p) => Die::Fatal(msg::prefix_not_found(&p)),
        WriteTreeError::BadMode => Die::Fatal(msg::ERROR_BUILDING_TREES.into()),
        WriteTreeError::Odb(e) => Die::Fatal(e.to_string()),
    })
}
