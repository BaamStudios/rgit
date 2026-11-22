// SPDX-License-Identifier: MIT
//! `git prune-packed` (Documentation/git-prune-packed.adoc): remove loose
//! objects that a local pack holds as well. `-n` prints the `rm -f`
//! commands instead. Progress output (`-q` turns it off) is not shown.

use crate::housekeeping::store;
use crate::messages::housekeeping as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(0);
    let quiet = Cell::new(0);
    let mut parser = Parser::new(
        msg::PRUNE_PACKED_USAGE,
        vec![
            Opt::dry_run(&dry_run, msg::PRUNE_PACKED_H_DRY_RUN),
            Opt::quiet(&quiet, msg::PRUNE_PACKED_H_QUIET),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_msg(msg::TOO_MANY_ARGUMENTS).into());
    }
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let odb = repo::odb_with_config(&setup, &config)?;
    let objects = rgit_odb::objects_dir(&setup.common_dir);
    let packs = store::local_packs(&odb, &[]);
    store::prune_packed(
        &objects,
        &objects,
        setup.format.object_format,
        &packs,
        dry_run.get() != 0,
    )?;
    Ok(0)
}
