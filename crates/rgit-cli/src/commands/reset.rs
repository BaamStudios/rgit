// SPDX-License-Identifier: MIT
//! `git reset` (Documentation/git-reset.adoc).
//!
//! Implemented: `--soft [<commit>]` (moves `HEAD`, records `ORIG_HEAD` and
//! the reflog entry `reset: moving to <commit>`). Not yet: `--mixed` (the
//! default), `--hard`, `--merge`, `--keep`, pathspecs.

use rgit_object::ObjectKind;
use rgit_refs::{RefUpdate, Transaction};

use crate::messages::reset as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let soft = Cell::new(false);
    let mixed = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag(None, "soft", &soft, msg::HELP_SOFT).noneg(),
            Opt::flag(None, "mixed", &mixed, msg::HELP_MIXED).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !soft.get() {
        return Err(Die::Fatal(msg::MODE_NOT_SUPPORTED.to_owned()));
    }
    let (rev, has_paths) = match args.iter().position(|a| a == "--") {
        Some(0) => (None, args.len() > 1),
        Some(1) => (Some(args[0].clone()), args.len() > 2),
        Some(_) => (Some(args[0].clone()), true),
        None => (args.first().cloned(), args.len() > 1),
    };

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = refs_support::Revs::new(ctx, &setup)?;

    let rev_name = rev.unwrap_or_else(|| "HEAD".to_owned());
    let target = match revs.resolve(&rev_name) {
        Some(id) => id,
        None if args.len() == 1 => return Err(Die::Fatal(msg::ambiguous(&rev_name))),
        None => return Err(Die::Fatal(msg::failed_to_resolve(&rev_name))),
    };
    if has_paths {
        return Err(Die::Fatal(msg::SOFT_WITH_PATHS.to_owned()));
    }
    let peeled = objects.peel(&target);
    let kind = peeled.as_ref().and_then(|id| objects.kind(id));
    let commit = match (peeled, kind) {
        (Some(id), Some(ObjectKind::Commit)) => id,
        (Some(id), Some(k)) => {
            eprintln!("{}", msg::wrong_kind(&id.to_string(), &k.to_string()));
            return Err(Die::Fatal(msg::not_a_commit(&rev_name)));
        }
        _ => return Err(Die::Fatal(msg::not_a_commit(&rev_name))),
    };

    let mut hooks = UpdateHooks::new(&setup, &config, &objects);
    let old = revs.resolve("HEAD");
    if let Some(old) = old {
        let update = RefUpdate::update("ORIG_HEAD", old, None)
            .no_deref(true)
            .message(msg::UPDATING_ORIG_HEAD);
        let mut tx = Transaction::new(&store);
        tx.push(update)
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| Die::Error(e.to_string(), 1))?;
    }
    let update = RefUpdate::update("HEAD", commit, old).message(msg::moving_to(&rev_name));
    let mut tx = Transaction::new(&store);
    tx.push(update)
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Error(e.to_string(), 1))?;
    Ok(0)
}
