// SPDX-License-Identifier: MIT
//! `git backfill` (Documentation/git-backfill.adoc): download the blobs a
//! partial clone is missing, in batches. In a repository without a promisor
//! remote nothing is missing, so there is nothing to do (as with git);
//! fetching from a promisor remote is #640.

use crate::messages::helpers as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let min_batch_size = Cell::new(0u64);
    let sparse = Cell::new(-1);
    let include_edges = Cell::new(1);
    let mut parser = Parser::new(
        msg::BACKFILL_USAGE,
        vec![
            Opt::unsigned(
                None,
                "min-batch-size",
                &min_batch_size,
                msg::BACKFILL_H_MIN_BATCH_SIZE,
            ),
            Opt::bool(None, "sparse", &sparse, msg::BACKFILL_H_SPARSE),
            Opt::bool(
                None,
                "include-edges",
                &include_edges,
                msg::BACKFILL_H_INCLUDE_EDGES,
            ),
        ],
    );
    parser.parse(argv)?;
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    if !crate::promisor::has_remote(&config) {
        return Ok(0);
    }
    Err(Die::Fatal(msg::BACKFILL_UNSUPPORTED.to_owned()))
}
