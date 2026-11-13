// SPDX-License-Identifier: MIT
//! `git merge-base` (Documentation/git-merge-base.adoc): best common
//! ancestors (`--all`, `--octopus`), `--independent` and `--is-ancestor`.
//! `--fork-point` is not implemented yet.

use std::io::Write;

use rgit_hash::ObjectId;
use rgit_revwalk::revision::{Error as RevError, Resolver};

use crate::messages::merge_base as msg;
use crate::prelude::*;
use crate::refs_support::Revs;
use crate::repo::{self, Mode};

const DEFAULT: i32 = 0;
const OCTOPUS: i32 = 1;
const INDEPENDENT: i32 = 2;
const IS_ANCESTOR: i32 = 3;
const FORK_POINT: i32 = 4;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let all = Cell::new(false);
    let mode = Cell::new(DEFAULT);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::cmdmode(None, "octopus", &mode, msg::HELP_OCTOPUS, OCTOPUS),
            Opt::cmdmode(None, "independent", &mode, msg::HELP_INDEPENDENT, INDEPENDENT),
            Opt::cmdmode(None, "is-ancestor", &mode, msg::HELP_IS_ANCESTOR, IS_ANCESTOR),
            Opt::cmdmode(None, "fork-point", &mode, msg::HELP_FORK_POINT, FORK_POINT),
        ],
    );
    let args = parser.parse(argv)?.args;
    let mode = mode.get();
    if all.get() {
        match mode {
            IS_ANCESTOR => return Err(Die::Fatal(msg::incompatible("--is-ancestor", "--all"))),
            INDEPENDENT => return Err(Die::Fatal(msg::incompatible("--independent", "--all"))),
            _ => {}
        }
    }
    let usage_ok = match mode {
        DEFAULT | IS_ANCESTOR => args.len() >= 2,
        FORK_POINT => (1..=2).contains(&args.len()),
        _ => true,
    };
    if !usage_ok {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    if mode == IS_ANCESTOR && args.len() != 2 {
        return Err(Die::Fatal(msg::IS_ANCESTOR_TWO.to_owned()));
    }
    if mode == FORK_POINT {
        return Err(Die::Fatal(msg::FORK_POINT_NOT_SUPPORTED.to_owned()));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let revs = Revs::new(ctx, &setup)?;
    revs.with_resolver(|r| {
        let mut commits = Vec::with_capacity(args.len());
        for a in &args {
            commits.push(commit_arg(r, a)?);
        }
        let lookup = |e: RevError| Die::Fatal(e.to_string());
        let (found, print_all) = match mode {
            IS_ANCESTOR => {
                let yes = r.is_ancestor(&commits[0], &commits[1]).map_err(lookup)?;
                return Ok(if yes { 0 } else { 1 });
            }
            INDEPENDENT => (r.independent(&commits).map_err(lookup)?, true),
            OCTOPUS => (r.octopus_merge_bases(&commits).map_err(lookup)?, all.get()),
            _ => (
                r.merge_bases_many(&commits[0], &commits[1..])
                    .map_err(lookup)?,
                all.get(),
            ),
        };
        if found.is_empty() {
            return Ok(1);
        }
        let n = if print_all { found.len() } else { 1 };
        let mut out = String::new();
        for c in &found[..n] {
            out.push_str(&c.to_hex());
            out.push('\n');
        }
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(out.as_bytes());
        let _ = o.flush();
        Ok(0)
    })
}

/// Resolve `name` to a commit like git's merge-base: unknown names and
/// non-commits end the command.
fn commit_arg(r: &Resolver<'_>, name: &str) -> Result<ObjectId, Die> {
    let print_diags = |r: &Resolver<'_>| {
        let mut err = std::io::stderr().lock();
        for d in r.take_diagnostics() {
            let _ = err.write_all(d.render().as_bytes());
        }
    };
    let oid = match r.resolve(name) {
        Ok(o) => o,
        Err(RevError::NotFound) => {
            print_diags(r);
            return Err(Die::Fatal(msg::not_a_valid_object(name)));
        }
        Err(e) => {
            print_diags(r);
            return Err(Die::Fatal(e.to_string()));
        }
    };
    print_diags(r);
    match r.peel_to_commit(&oid) {
        Ok(c) => Ok(c),
        Err(RevError::NotFound) => {
            print_diags(r);
            Err(Die::Fatal(msg::not_a_valid_commit(name)))
        }
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}
