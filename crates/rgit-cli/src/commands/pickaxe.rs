// SPDX-License-Identifier: MIT
//! `git pickaxe`: an undocumented synonym for [`git blame`](super::blame)
//! (it answers `-h` with the usage of `blame`, as the `git` binary does).

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::blame::main(ctx, argv, false)
}
