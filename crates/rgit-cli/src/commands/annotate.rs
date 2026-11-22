// SPDX-License-Identifier: MIT
//! `git annotate` (Documentation/git-annotate.adoc): `git blame` with the
//! annotate output format (`git blame -c`).

use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    super::blame::main(ctx, argv, true)
}
