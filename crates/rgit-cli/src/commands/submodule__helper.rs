// SPDX-License-Identifier: MIT
//! `git submodule--helper`: the internal helper of `git submodule`. Only
//! `get-default-remote <path>` is provided; the subcommands of `git
//! submodule` itself are built in.

use crate::commands::submodule::common::{self, Super};
use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        msg::USAGE_HELPER,
        vec![Opt::subcommand("get-default-remote")],
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    let mut parser = Parser::new(msg::USAGE_DEFAULT_REMOTE, vec![]);
    let rest = parser.parse(&parsed.args)?.args;
    if rest.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let sup = Super::open(ctx, None)?;
    let path = common::prefixed_path(&sup, &rest[0])?;
    let dir = sup.work_tree.join(&path);
    let git_dir = common::sub_git_dir(&dir)
        .filter(|_| common::is_populated(&dir) && !path.is_empty())
        .ok_or_else(|| Die::Fatal(msg::path_not_submodule(&rest[0])))?;
    println!("{}", common::sub_default_remote(&git_dir));
    Ok(0)
}
