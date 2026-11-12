// SPDX-License-Identifier: MIT
//! `git rev-list` (Documentation/git-rev-list.adoc): list commits reachable
//! from the given revisions, see [`crate::history`].

use crate::history::{self, Command};
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    history::main(ctx, Command::RevList, &argv[1..])
}
