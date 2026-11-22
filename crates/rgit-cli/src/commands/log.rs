// SPDX-License-Identifier: MIT
//! `git log` (Documentation/git-log.adoc): show commits with the built-in
//! pretty formats, see [`crate::history`], through the pager (`pager.log`)
//! and coloured (`color.diff`, `--color`), with decorations and the
//! `--graph` drawing ([`crate::history::graph`]). Diffs come later.

use crate::history::{self, Command};
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    history::main(ctx, Command::Log, &argv[1..])
}
