// SPDX-License-Identifier: MIT
//! `git fsck-objects` (Documentation/git-fsck-objects.adoc): the old name
//! of [`git fsck`](super::fsck), kept as a synonym.

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::fsck::run(ctx, argv)
}
