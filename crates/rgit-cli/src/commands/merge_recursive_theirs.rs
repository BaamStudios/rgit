// SPDX-License-Identifier: MIT
//! `git merge-recursive-theirs <base>... -- <head> <remote>`: an
//! undocumented strategy program of git. Observed from the `git` binary, it
//! merges like [`merge-recursive`](super::merge_recursive) (conflicts are
//! left in the index and work tree, no side is favoured); only its usage
//! differs.

use crate::messages::merge_plumbing as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::merge_recursive::run_with(ctx, argv, None, msg::MERGE_RECURSIVE_THEIRS_USAGE)
}
