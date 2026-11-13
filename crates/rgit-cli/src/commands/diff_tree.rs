// SPDX-License-Identifier: MIT
//! `git diff-tree` (Documentation/git-diff-tree.adoc) for two tree-ishes
//! with optional pathspecs, in all formats of [`crate::diff::output`].
//! No rename detection, no single-commit form, no `--stdin`.

use rgit_object::ObjectKind;

use crate::diff::collect::TreeDiff;
use crate::diff::output::{Output, finish};
use crate::diff::{self, options::usage_exit};
use crate::messages::diff as dmsg;
use crate::messages::diff_tree as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(code) = diff::help(argv, &format!("usage: {}\n{}", msg::USAGE[0], dmsg::COMMON_OPTIONS)) {
        return Ok(code);
    }
    let usage = || usage_exit(format!("usage: {}\n{}", msg::USAGE[0], dmsg::COMMON_OPTIONS));
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            "--root" | "--no-commit-id" => Ok(true),
            "--stdin" | "-c" | "--cc" | "-m" | "--merge-base" => {
                Err(Die::Fatal(dmsg::not_supported(a)))
            }
            _ => Ok(false),
        },
        |_| usage(),
    )?;
    let opts = parsed.opts;
    let mut args = parsed.args;
    let revs = parsed.dashdash_at.unwrap_or(args.len()).min(2);
    if revs != 2 {
        return Err(Die::Fatal(dmsg::not_supported("diff-tree with one tree-ish")));
    }
    let formats = opts.formats(false)?;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let mut trees = Vec::new();
    for spec in args.drain(..2) {
        let id = repo
            .resolve(&spec)
            .and_then(|id| repo.peel_to(id, ObjectKind::Tree))
            .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
        trees.push(id);
    }
    let repo = repo;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    let mut w = TreeDiff {
        repo: &repo,
        pathspec: &pathspec,
        // -p and -t imply -r
        recursive: opts.recursive || formats.patch || opts.show_trees || formats.needs_content(),
        show_trees: opts.show_trees,
        pairs: Vec::new(),
    };
    w.walk(Some(&trees[0]), Some(&trees[1]), b"")?;
    let pairs = std::mem::take(&mut w.pairs);
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false);
    out.set_color(&opts)?;
    let (bytes, changed) = out.render(pairs)?;
    Ok(finish(&bytes, changed, &opts))
}
