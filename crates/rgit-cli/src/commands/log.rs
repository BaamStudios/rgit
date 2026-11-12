// SPDX-License-Identifier: MIT
//! `git log` (Documentation/git-log.adoc): show commits with the built-in
//! pretty formats, see [`crate::history`]. Diffs, decorations, `--graph`
//! and the pager come later.

use crate::history::{self, Command};
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    history::main(ctx, Command::Log, &argv[1..])
}
