// SPDX-License-Identifier: MIT
//! `git init-db`: another name for `git init` (Documentation/git-init-db.adoc).

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::init::run(ctx, argv)
}
