// SPDX-License-Identifier: MIT
//! `git submodule` (Documentation/git-submodule.adoc, gitsubmodules.adoc,
//! gitmodules.adoc, config/submodule.adoc): add, status, init, deinit,
//! update (checkout, rebase, merge, `!command`, none; `--init`,
//! `--remote`, `--recursive`, `--depth`, `--no-fetch`, `--force`),
//! set-branch, set-url, summary, foreach, sync and absorbgitdirs.
//!
//! Work inside a submodule (clone, fetch, checkout, rebase, merge,
//! describe, log) runs this binary as a child process in the submodule's
//! work tree with the superproject's repository variables removed, like
//! git's own submodule commands do. Recursion runs the same subcommand in
//! the submodule with the hidden `--super-prefix` option, which sets the
//! path shown in messages.
//!
//! Not yet: `--reference`/`--dissociate` (clone does not support them),
//! `--filter`, `--ref-format`, `--jobs` (accepted, sequential).

#[path = "submodule/add.rs"]
mod add;
#[path = "submodule/common.rs"]
pub(crate) mod common;
#[path = "submodule/misc.rs"]
mod misc;
#[path = "submodule/status.rs"]
mod status;
#[path = "submodule/summary.rs"]
mod summary;
#[path = "submodule/update.rs"]
mod update;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

const SUBCOMMANDS: &[&str] = &[
    "add",
    "foreach",
    "init",
    "deinit",
    "update",
    "set-branch",
    "set-url",
    "status",
    "summary",
    "sync",
    "absorbgitdirs",
];

pub(crate) fn usage_error() -> Die {
    Die::Output(rgit_options::Exit {
        code: 1,
        stdout: String::new(),
        stderr: msg::USAGE.to_owned(),
    })
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut quiet = false;
    let mut cached = false;
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-q" | "--quiet" => quiet = true,
            "--cached" => cached = true,
            "-h" | "--help-all" => {
                return Err(Die::Output(rgit_options::Exit {
                    code: 0,
                    stdout: msg::USAGE.to_owned(),
                    stderr: String::new(),
                }));
            }
            a if a.starts_with('-') => return Err(usage_error()),
            _ => break,
        }
        i += 1;
    }
    let (sub, rest): (&str, &[String]) = match argv.get(i) {
        Some(s) if SUBCOMMANDS.contains(&s.as_str()) => (s.as_str(), &argv[i + 1..]),
        Some(_) => return Err(usage_error()),
        None => ("status", &[]),
    };
    if sub != "absorbgitdirs" && common::wants_help(rest) {
        return Err(usage_error());
    }
    if cached && !matches!(sub, "status" | "summary") {
        return Err(usage_error());
    }
    // The options given before the subcommand go to it.
    let mut args: Vec<String> = vec![sub.to_owned()];
    if quiet {
        args.push("--quiet".into());
    }
    if cached {
        args.push("--cached".into());
    }
    args.extend(rest.iter().cloned());
    run_sub(ctx, sub, &args)
}

/// Run subcommand `sub`; `args[0]` is its name.
pub fn run_sub(ctx: &mut Context, sub: &str, args: &[String]) -> CmdResult {
    match sub {
        "add" => add::run(ctx, args),
        "foreach" => misc::foreach(ctx, args),
        "init" => update::init_cmd(ctx, args),
        "deinit" => misc::deinit(ctx, args),
        "update" => update::run(ctx, args),
        "set-branch" => misc::set_branch(ctx, args),
        "set-url" => misc::set_url(ctx, args),
        "status" => status::run(ctx, args),
        "summary" => summary::run(ctx, args),
        "sync" => misc::sync(ctx, args),
        _ => misc::absorbgitdirs(ctx, args),
    }
}
