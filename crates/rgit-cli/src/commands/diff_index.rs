// SPDX-License-Identifier: MIT
//! `git diff-index` (Documentation/git-diff-index.adoc): a tree against the
//! index (`--cached`) or the work tree. No `--merge-base`, no `-m`.

use rgit_object::ObjectKind;

use crate::diff::collect::{self, WorkTree};
use crate::diff::output::{Output, finish};
use crate::diff::{self, options::usage_exit};
use crate::messages::diff as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

fn usage() -> Die {
    usage_exit(format!("{}{}", msg::DIFF_INDEX_USAGE, msg::COMMON_OPTIONS))
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(code) = diff::help(argv, &format!("{}{}", msg::DIFF_INDEX_USAGE, msg::COMMON_OPTIONS)) {
        return Ok(code);
    }
    let mut cached = false;
    let parsed = diff::parse_args(
        &argv[1..],
        |a| match a {
            "--cached" => {
                cached = true;
                Ok(true)
            }
            "-m" | "--merge-base" => Err(Die::Fatal(msg::not_supported(a))),
            _ => Ok(false),
        },
        |_| usage(),
    )?;
    let opts = parsed.opts;
    let mut args = parsed.args;
    if args.is_empty() || parsed.dashdash_at == Some(0) {
        return Err(usage());
    }
    let formats = opts.formats(false)?;
    let mut repo = Repo::open(ctx, Mode::RequiredQuiet)?;
    let spec = args.remove(0);
    let id = repo
        .resolve(&spec)
        .ok_or_else(|| Die::Fatal(crate::messages::ambiguous_argument(&spec)))?;
    let tree = repo
        .peel_to(id, ObjectKind::Tree)
        .ok_or_else(|| Die::Fatal(msg::bad_tree_object(&spec)))?;
    let repo = repo;
    let pathspec = diff::pathspec(ctx, &repo, &args)?;
    let (index, mtime) = repo.read_index()?;
    let pairs = if cached {
        collect::index(&repo, &tree, &index, &pathspec, None, false)?
    } else {
        let root = repo.work_tree()?;
        let checker = repo.checker(mtime)?;
        let wt = WorkTree {
            root: &root,
            checker: &checker,
            porcelain: false,
            hash: repo.hash,
        };
        collect::index(&repo, &tree, &index, &pathspec, Some(&wt), false)?
    };
    let mut out = Output::new(Some(&repo), repo.hash, &opts, formats, false);
    out.set_color(&opts)?;
    let (bytes, changed) = out.render(pairs)?;
    Ok(finish(&bytes, changed, &opts))
}
