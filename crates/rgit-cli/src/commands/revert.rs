// SPDX-License-Identifier: MIT
//! `git revert` (Documentation/git-revert.adoc) on the sequencer
//! ([`crate::sequencer`]); the arguments are handled with `cherry-pick`'s.

use crate::prelude::*;
use crate::sequencer::Action;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::cherry_pick::run_action(ctx, argv, Action::Revert)
}
