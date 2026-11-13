// SPDX-License-Identifier: MIT
//! `git log` (Documentation/git-log.adoc): show commits with the built-in
//! pretty formats, see [`crate::history`], through the pager (`pager.log`)
//! and coloured (`color.diff`, `--color`). Diffs, decorations and `--graph`
//! come later.

use crate::history::{self, Command};
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    history::main(ctx, Command::Log, &argv[1..])
}
