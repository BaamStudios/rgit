// SPDX-License-Identifier: MIT
//! `git show` (Documentation/git-show.adoc): commits with their diff
//! (merges: combined diffs need #148), annotated tags, trees and blobs, see
//! [`crate::history`].

use crate::history;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    history::show_main(ctx, &argv[1..])
}
