// SPDX-License-Identifier: MIT
//! `git update-server-info` (Documentation/git-update-server-info.adoc):
//! rewrite `info/refs` and `objects/info/packs` for dumb transports.
//! Gitea runs it after creating a repository and after every push.
//! A file whose content stays the same is not rewritten unless `-f`.

use crate::housekeeping::{server_info, store};
use crate::messages::housekeeping as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let force = Cell::new(0);
    let mut parser = Parser::new(
        msg::UPDATE_SERVER_INFO_USAGE,
        vec![Opt::bool('f', "force", &force, msg::UPDATE_SERVER_INFO_H_FORCE)],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let odb = repo::odb_with_config(&setup, &config)?;
    let packs = store::local_packs(&odb, &[]);
    let shared = crate::refs_support::shared_repository(&config);
    match server_info::update(&setup, &odb, &packs, shared, force.get() != 0) {
        Ok(()) => Ok(0),
        Err((path, e)) => {
            let path = path.to_string_lossy();
            let path = path.strip_prefix("./").unwrap_or(&path);
            Err(Die::Error(
                msg::unable_to_update(path, &crate::global::strerror(&e)),
                1,
            ))
        }
    }
}
