// SPDX-License-Identifier: MIT
//! `git merge-subtree <base>... -- <head> <remote>` (Documentation/
//! merge-strategies.adoc, `subtree`): the strategy program behind
//! `merge -s subtree`, [`merge-recursive`](super::merge_recursive) with
//! the trees shifted to match (exit 0 when clean, 1 with conflicts).

use crate::messages::merge_plumbing as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::merge_recursive::run_with(ctx, argv, Some("subtree"), msg::MERGE_SUBTREE_USAGE)
}
