// SPDX-License-Identifier: MIT
//! `git whatchanged` (Documentation/git-whatchanged.adoc): deprecated; it
//! only runs with `--i-still-use-this` and is then `git log --raw
//! --no-merges` ([`crate::history::whatchanged_main`]).

use crate::history;
use crate::messages::deprecated as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if matches!(args.first().map(String::as_str), Some("-h" | "--help-all")) {
        return history::whatchanged_main(ctx, args);
    }
    let (flag, rest): (Vec<&String>, Vec<&String>) =
        args.iter().partition(|a| *a == msg::STILL_USE_THIS);
    if flag.is_empty() {
        eprint!("{}", msg::removal_notice("whatchanged", msg::WHATCHANGED_HINT));
        return Err(Die::Fatal(msg::REFUSING_TO_RUN.to_owned()));
    }
    let rest: Vec<String> = rest.into_iter().cloned().collect();
    history::whatchanged_main(ctx, &rest)
}
