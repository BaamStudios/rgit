// SPDX-License-Identifier: MIT
//! `git diff-files` (Documentation/git-diff-files.adoc): the index against
//! the work tree. Unmerged paths: `-0`..`-3` choose the stage compared,
//! `-c`/`--cc` (and `-p`) show combined diffs of stages 2 and 3.

use crate::diff::collect::{self, WorkTree};
use crate::diff::output::Output;
use crate::diff::{self, options::usage_exit};
use crate::messages::diff as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

fn usage() -> Die {
    usage_exit(format!("{}{}", msg::DIFF_FILES_USAGE, msg::COMMON_OPTIONS))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(code) = diff::help(argv, &format!("{}{}", msg::DIFF_FILES_USAGE, msg::COMMON_OPTIONS)) {
        return Ok(code);
    }
    let mut stage: Option<usize> = None;
    let mut combined: Option<bool> = None;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| {
            // Only silences errors about missing files, which are not reported.
            if a == "-q" {
                return Ok(true);
            }
            if let Some(n) = diff::stage_option(a) {
                stage = Some(n);
                return Ok(true);
            }
            match a {
                "-c" => combined = Some(false),
                "--cc" => combined = Some(true),
                _ => return Ok(false),
            }
            Ok(true)
        },
        |_| usage(),
    )?;
    let mut opts = parsed.opts;
    // `-c` and `--cc` imply a patch.
    let any = opts.raw
        || opts.patch
        || opts.name_only
        || opts.name_status
        || opts.numstat
        || opts.stat
        || opts.shortstat
        || opts.summary;
    if combined.is_some() && !any {
        opts.patch = true;
    }
    let formats = opts.formats(false)?;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    opts.resolve_objects(&mut repo)?;
    let pathspec = diff::pathspec(ctx, &repo, &parsed.args)?;
    let (pathspec, attr_filter) = diff::attr_split(ctx, &mut repo, pathspec)?;
    let repo = repo;
    let root = repo.work_tree()?;
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    let submodules = crate::submodules::IgnoreRules::for_index(&repo, &index, opts.ignore_submodules()?, false)?;
    let wt = WorkTree {
        root: &root,
        checker: &checker,
        hash: repo.hash,
        submodules: submodules.as_ref(),
        dirty: Default::default(),
    };
    let mut pairs = collect::files(&index, &pathspec, &wt, false)?;
    if opts.copies_harder {
        pairs = collect::with_unmodified(pairs, collect::index_files(&index, &pathspec));
    }
    crate::submodules::IgnoreRules::drop_ignored(&repo, &mut pairs, opts.ignore_submodules()?, false)?;
    diff::attr_retain(&repo, &mut pairs, attr_filter)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
    out.set_color(&opts)?;
    let unmerged = collect::unmerged(&index, &pathspec, &wt)?;
    let how = diff::unmerged_as(stage, combined, formats.patch);
    let (files, pairs) = crate::diff::combined::worktree(&repo, &out, &unmerged, pairs, how)?;
    let mut bytes = Vec::new();
    crate::diff::combined::worktree_output(&repo, &out, &files, combined != Some(false), &mut bytes);
    let (rest, changed) = out.render(pairs)?;
    bytes.extend_from_slice(&rest);
    Ok(out.finish(&bytes, changed || !files.is_empty(), &opts))
}
