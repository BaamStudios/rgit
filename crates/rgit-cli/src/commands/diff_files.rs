// SPDX-License-Identifier: MIT
//! `git diff-files` (Documentation/git-diff-files.adoc): the index against
//! the work tree. No combined diffs (`-c`, `--cc`, `-0`..`-3`).

use crate::diff::collect::{self, WorkTree};
use crate::diff::output::{Output, finish};
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
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            // Only silences errors about missing files, which are not reported.
            "-q" => Ok(true),
            "-0" | "-1" | "-2" | "-3" | "--base" | "--ours" | "--theirs" | "-c" | "--cc" => {
                Err(Die::Fatal(msg::not_supported(a)))
            }
            _ => Ok(false),
        },
        |_| usage(),
    )?;
    let opts = parsed.opts;
    let formats = opts.formats(false)?;
    let repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let pathspec = diff::pathspec(ctx, &repo, &parsed.args)?;
    let root = repo.work_tree()?;
    let (index, mtime) = repo.read_index()?;
    let checker = repo.checker(mtime)?;
    let wt = WorkTree {
        root: &root,
        checker: &checker,
        porcelain: false,
        hash: repo.hash,
    };
    let pairs = collect::files(&index, &pathspec, &wt, false)?;
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false);
    out.combined_unmerged = true;
    let (bytes, changed) = out.render(pairs)?;
    Ok(finish(&bytes, changed, &opts))
}
