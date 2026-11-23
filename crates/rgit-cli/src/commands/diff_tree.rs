// SPDX-License-Identifier: MIT
//! `git diff-tree` (Documentation/git-diff-tree.adoc) for two tree-ishes
//! with optional pathspecs, in all formats of [`crate::diff::output`],
//! and `--merge-base`. No single-commit form, no `--stdin`.

use rgit_object::ObjectKind;

use crate::diff::collect::TreeDiff;
use crate::diff::output::Output;
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
    let mut merge_base = false;
    let mut stdin = false;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            "--root" | "--no-commit-id" => Ok(true),
            "--merge-base" => {
                merge_base = true;
                Ok(true)
            }
            "--stdin" => {
                stdin = true;
                Ok(true)
            }
            "-c" | "--cc" | "-m" => Err(Die::Fatal(dmsg::not_supported(a))),
            _ => Ok(false),
        },
        |_| usage(),
    )?;
    let mut opts = parsed.opts;
    let mut args = parsed.args;
    if merge_base && stdin {
        return Err(Die::Fatal(dmsg::STDIN_WITH_MERGE_BASE.to_owned()));
    }
    if stdin {
        return Err(Die::Fatal(dmsg::not_supported("--stdin")));
    }
    if merge_base {
        let before = parsed.dashdash_at.unwrap_or(args.len());
        if args[..before].iter().any(|a| a.contains("..")) {
            return Err(Die::Fatal(dmsg::MERGE_BASE_WITH_RANGES.to_owned()));
        }
        if before < 2 {
            return Err(Die::Fatal(dmsg::MERGE_BASE_TWO_COMMITS.to_owned()));
        }
    }
    let revs = parsed.dashdash_at.unwrap_or(args.len()).min(2);
    if revs != 2 {
        return Err(Die::Fatal(dmsg::not_supported("diff-tree with one tree-ish")));
    }
    let formats = opts.formats(false)?;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    if merge_base
        && parsed.dashdash_at.unwrap_or(args.len()) > 2
        && repo.resolve(&args[2]).is_some()
    {
        return Err(Die::Fatal(dmsg::MERGE_BASE_TWO_COMMITS.to_owned()));
    }
    let mut ids = Vec::new();
    for spec in args.drain(..2) {
        let id = repo
            .resolve(&spec)
            .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
        ids.push((spec, id));
    }
    if merge_base {
        ids[0].1 = diff::single_merge_base(ctx, &mut repo, ids[0].1, ids[1].1)?;
    }
    let mut trees = Vec::new();
    for (spec, id) in &ids {
        let id = repo
            .peel_to(*id, ObjectKind::Tree)
            .ok_or_else(|| Die::Fatal(msg::bad_object(spec)))?;
        trees.push(id);
    }
    opts.resolve_objects(&mut repo)?;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    diff::check_max_depth(&opts, &pathspec)?;
    let (pathspec, attr_filter) = diff::attr_split(ctx, &mut repo, pathspec)?;
    let repo = repo;
    let mut w = TreeDiff {
        repo: &repo,
        pathspec: &pathspec,
        // -p and -t imply -r
        recursive: opts.recursive
            || formats.patch
            || opts.show_trees
            || formats.needs_content()
            || opts.max_depth_given,
        show_trees: opts.show_trees,
        max_depth: opts.max_depth,
        pairs: Vec::new(),
    };
    w.walk(Some(&trees[0]), Some(&trees[1]), b"")?;
    let mut pairs = std::mem::take(&mut w.pairs);
    if opts.copies_harder {
        let files = crate::diff::collect::tree_files(&repo, &trees[0], &pathspec)?;
        pairs = crate::diff::collect::with_unmodified(pairs, files);
    }
    diff::attr_retain(&repo, &mut pairs, attr_filter)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
    out.set_color(&opts)?;
    let (bytes, changed) = out.render(pairs)?;
    Ok(out.finish(&bytes, changed, &opts))
}
