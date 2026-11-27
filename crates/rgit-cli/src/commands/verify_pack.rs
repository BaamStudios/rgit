// SPDX-License-Identifier: MIT
//! `git verify-pack` (Documentation/git-verify-pack.adoc): check packs by
//! re-indexing each one with `index-pack --verify` in a child process
//! (`--verify-stat` / `--verify-stat-only` for `-v` / `-s`, which print the
//! object list and the delta chain histogram).

use crate::argv::CommandArgs as _;
use std::process::Command;

use crate::messages::verify_pack as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let stat_only = Cell::new(false);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::VERIFY_PACK_USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::flag('s', "stat-only", &stat_only, msg::HELP_STAT_ONLY),
            Opt::string(None, "object-format", &format, "hash", msg::HELP_OBJECT_FORMAT),
        ],
    );
    let args = parser.parse(argv)?.args;
    if args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let exe = std::env::current_exe().map_err(|e| Die::Fatal(e.to_string()))?;
    let mut failed = false;
    for name in &args {
        let base = name
            .strip_suffix(".idx")
            .or_else(|| name.strip_suffix(".pack"))
            .unwrap_or(name);
        let pack = format!("{base}.pack");
        let mut cmd = Command::new(&exe);
        cmd.raw_arg("index-pack");
        cmd.raw_arg(match (verbose.get(), stat_only.get()) {
            (_, true) => "--verify-stat-only",
            (true, false) => "--verify-stat",
            (false, false) => "--verify",
        });
        if let Some(f) = format.borrow().as_deref() {
            cmd.raw_arg(format!("--object-format={f}"));
        }
        cmd.raw_arg(&pack).envs(ctx.child_env());
        let ok = cmd.status().is_ok_and(|s| s.success());
        if verbose.get() || stat_only.get() {
            if !ok {
                println!("{}", msg::pack_bad(&pack));
            } else if !stat_only.get() {
                println!("{}", msg::pack_ok(&pack));
            }
        }
        failed |= !ok;
    }
    Ok(i32::from(failed))
}
