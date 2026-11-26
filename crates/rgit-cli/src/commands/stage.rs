// SPDX-License-Identifier: MIT
//! `git stage` (Documentation/git-stage.adoc): a synonym for
//! [`git add`](super::add).

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::add::run(ctx, argv)
}
