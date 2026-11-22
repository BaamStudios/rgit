// SPDX-License-Identifier: MIT
//! `git diff-tree` (Documentation/git-diff-tree.adoc) for two tree-ishes
//! with optional pathspecs, in all formats of [`crate::diff::output`],
//! and `--merge-base`; for one commit its diff against its parent
//! (`--root`, `--no-commit-id`), for a merge with `-m` against every
//! parent and with `-c`/`--cc` the combined diff (`diff/combined.rs`).
//! No `--stdin`.

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
    let mut root = false;
    let mut no_commit_id = false;
    let mut merges: Option<&'static str> = None;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            "--root" => {
                root = true;
                Ok(true)
            }
            "--no-commit-id" => {
                no_commit_id = true;
                Ok(true)
            }
            "-c" => {
                merges = Some("-c");
                Ok(true)
            }
            "--cc" => {
                merges = Some("--cc");
                Ok(true)
            }
            "-m" => {
                merges = Some("-m");
                Ok(true)
            }
            "--merge-base" => {
                merge_base = true;
                Ok(true)
            }
            "--stdin" => {
                stdin = true;
                Ok(true)
            }
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
    if revs == 1 || (revs == 2 && !merge_base && args.len() >= 2 && single_commit(ctx, &args)?) {
        let flags = Single {
            root,
            no_commit_id,
            merges,
        };
        return one_commit(ctx, opts, args, parsed.dashdash_at, &flags);
    }
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
    let repo = repo;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    diff::check_max_depth(&opts, &pathspec)?;
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
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
    out.set_color(&opts)?;
    let (bytes, changed) = out.render(pairs)?;
    Ok(out.finish(&bytes, changed, &opts))
}

/// The flags of the single-commit form.
struct Single {
    root: bool,
    no_commit_id: bool,
    merges: Option<&'static str>,
}

/// Whether `<a> <b>` is a commit followed by a path (the single-commit
/// form with paths), which git decides by whether `b` names an object.
fn single_commit(ctx: &Context, args: &[String]) -> Result<bool, Die> {
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    Ok(repo.resolve(&args[1]).is_none() && repo.resolve(&args[0]).is_some())
}

/// `diff-tree <commit> [<path>...]`.
fn one_commit(
    ctx: &Context,
    mut opts: crate::diff::options::DiffOpts,
    mut args: Vec<String>,
    dashdash: Option<usize>,
    flags: &Single,
) -> CmdResult {
    let spec = args.remove(0);
    let _ = dashdash;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let id = repo
        .resolve(&spec)
        .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
    let commit_id = repo
        .peel_to(id, ObjectKind::Commit)
        .ok_or_else(|| Die::Fatal(msg::bad_object(&spec)))?;
    let Some(rgit_object::Object::Commit(commit)) = repo.read_object(&commit_id) else {
        return Err(Die::Fatal(msg::bad_object(&spec)));
    };
    let parents = commit
        .parents()
        .map_err(|_| Die::Fatal(msg::bad_object(&spec)))?;
    let tree = commit
        .tree()
        .map_err(|_| Die::Fatal(msg::bad_object(&spec)))?;
    let mut parent_trees = Vec::new();
    for p in &parents {
        parent_trees.push(
            repo.peel_to(*p, ObjectKind::Tree)
                .ok_or_else(|| Die::Fatal(msg::bad_object(&p.to_hex())))?,
        );
    }
    // --cc implies a patch.
    if flags.merges == Some("--cc") && !opts.raw && !opts.stat && !opts.numstat {
        opts.patch = true;
    }
    let formats = opts.formats(false)?;
    opts.resolve_objects(&mut repo)?;
    let repo = repo;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    let header = format!("{}\n", commit_id.to_hex());
    let mut out_all: Vec<u8> = Vec::new();
    let pair_diff = |old: Option<rgit_hash::ObjectId>| -> Result<Vec<u8>, Die> {
        let mut w = TreeDiff {
            repo: &repo,
            pathspec: &pathspec,
            recursive: opts.recursive
                || formats.patch
                || opts.show_trees
                || formats.needs_content()
                || opts.max_depth_given,
            show_trees: opts.show_trees,
            max_depth: opts.max_depth,
            pairs: Vec::new(),
        };
        w.walk(old.as_ref(), Some(&tree), b"")?;
        let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
        out.set_color(&opts)?;
        let (bytes, _) = out.render(std::mem::take(&mut w.pairs))?;
        Ok(bytes)
    };
    let emit = |bytes: Vec<u8>, out_all: &mut Vec<u8>| {
        if bytes.is_empty() && !opts.no_output {
            return;
        }
        if !flags.no_commit_id {
            out_all.extend_from_slice(header.as_bytes());
        }
        out_all.extend_from_slice(&bytes);
    };
    match parents.len() {
        0 => {
            if flags.root {
                let bytes = pair_diff(None)?;
                emit(bytes, &mut out_all);
            }
        }
        1 => {
            let bytes = pair_diff(Some(parent_trees[0]))?;
            emit(bytes, &mut out_all);
        }
        _ => match flags.merges {
            None => {}
            Some("-m") => {
                for t in &parent_trees {
                    let bytes = pair_diff(Some(*t))?;
                    emit(bytes, &mut out_all);
                }
            }
            Some(mode) => {
                if formats.patch || formats.raw {
                    let files: Vec<_> = crate::diff::combined::collect(&repo, &parent_trees, &tree, false)
                        .into_iter()
                        .filter(|f| pathspec.is_empty() || pathspec.matches(&f.path))
                        .collect();
                    let quote = repo.quote_path();
                    let mut bytes = Vec::new();
                    if formats.raw {
                        let abbrev = match opts.abbrev {
                            Some(Some(n)) => Some(n),
                            Some(None) => Some(repo.default_abbrev()),
                            None => None,
                        };
                        crate::diff::combined::raw(&repo, &files, abbrev, quote, &mut bytes);
                    }
                    if formats.patch {
                        let diff = rgit_diff::DiffOptions::default();
                        let popts = crate::diff::combined::PatchOptions {
                            dense: mode == "--cc",
                            abbrev: repo.default_abbrev(),
                            quote,
                            diff: &diff,
                        };
                        crate::diff::combined::patch(&repo, &files, &popts, &mut bytes);
                    }
                    emit(bytes, &mut out_all);
                } else {
                    // Other formats compare with the first parent.
                    let bytes = pair_diff(Some(parent_trees[0]))?;
                    emit(bytes, &mut out_all);
                }
            }
        },
    }
    let mut stdout = std::io::stdout().lock();
    use std::io::Write as _;
    let _ = stdout.write_all(&out_all);
    let _ = stdout.flush();
    Ok(0)
}
