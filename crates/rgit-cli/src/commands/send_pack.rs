// SPDX-License-Identifier: MIT
//! `git send-pack` (Documentation/git-send-pack.adoc): push named refs to a
//! repository given as a URL or path, with `git push`'s matching, checks
//! and status table but without its porcelain (no remote configuration,
//! hooks, hints or remote-tracking updates).

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::push::run_send_pack(ctx, argv)
}
