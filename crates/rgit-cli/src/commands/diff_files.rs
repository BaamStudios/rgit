// SPDX-License-Identifier: MIT
//! `git diff-files` (Documentation/git-diff-files.adoc): the index against
//! the work tree. No combined diffs (`-c`, `--cc`).

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
    // The conflict stage unmerged paths are compared with; without one a
    // patch is a combined diff and other formats use "ours" (`-2`).
    let mut stage = None;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| {
            stage = match a {
                // Only silences errors about missing files, which are not reported.
                "-q" => return Ok(true),
                "-0" => Some(0),
                "-1" | "--base" => Some(1),
                "-2" | "--ours" => Some(2),
                "-3" | "--theirs" => Some(3),
                "-c" | "--cc" => return Err(Die::Fatal(msg::not_supported(a))),
                _ => return Ok(false),
            };
            Ok(true)
        },
        |_| usage(),
    )?;
    let mut opts = parsed.opts;
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
    let mut pairs = collect::files_stage(&index, &pathspec, &wt, false, stage.unwrap_or(2))?;
    if opts.copies_harder {
        pairs = collect::with_unmodified(pairs, collect::index_files(&index, &pathspec));
    }
    crate::submodules::IgnoreRules::drop_ignored(&repo, &mut pairs, opts.ignore_submodules()?, false)?;
    diff::attr_retain(&repo, &mut pairs, attr_filter)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false)?;
    out.set_color(&opts)?;
    out.combined_unmerged = stage.is_none();
    let (bytes, changed) = out.render(pairs)?;
    Ok(out.finish(&bytes, changed, &opts))
}
