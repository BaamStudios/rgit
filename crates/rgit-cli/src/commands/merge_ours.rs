// SPDX-License-Identifier: MIT
//! `git merge-ours` (Documentation/merge-strategies.adoc, `ours`): the
//! strategy program behind `merge -s ours`. The result is the current
//! tree, so it only checks that the index matches `HEAD`: exit 0 when it
//! does, 2 when it does not. Its arguments are ignored (observed).

use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if argv.len() == 2 && matches!(argv[1].as_str(), "-h" | "--help-all") {
        println!("{}", msg::MERGE_OURS_USAGE);
        return Ok(0);
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    if repo.refs.resolve("HEAD").is_none() {
        if let Some(t) = repo.refs.head_target() {
            let branch = t.strip_prefix("refs/heads/").unwrap_or(&t);
            return Err(Die::Fatal(crate::messages::rev_list::unborn_branch(branch)));
        }
    }
    let git = super::merge_one_file::Plumbing::at_top(ctx, &repo);
    let code = git.run(["diff-index", "--quiet", "--cached", "HEAD", "--"])?;
    Ok(if code == 0 { 0 } else { 2 })
}
