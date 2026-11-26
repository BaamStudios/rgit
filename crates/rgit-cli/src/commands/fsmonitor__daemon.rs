// SPDX-License-Identifier: MIT
//! `git fsmonitor--daemon` (Documentation/git-fsmonitor--daemon.adoc): the
//! built-in file system monitor. rgit has no daemon yet (#118): `start` and
//! `run` say so, `status` and `stop` report that none is running, which is
//! always true for rgit.

use crate::messages::helpers as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let detach = Cell::new(0);
    let ipc_threads = Cell::new(0);
    let start_timeout = Cell::new(0);
    let mut parser = Parser::new(
        msg::FSMONITOR_USAGE,
        vec![
            Opt::bool(None, "detach", &detach, msg::FSMONITOR_H_DETACH),
            Opt::integer(None, "ipc-threads", &ipc_threads, msg::FSMONITOR_H_IPC_THREADS),
            Opt::integer(
                None,
                "start-timeout",
                &start_timeout,
                msg::FSMONITOR_H_START_TIMEOUT,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    let Some(sub) = args.first() else {
        return Err(parser.usage_error().into());
    };
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    match sub.as_str() {
        "start" | "run" => Err(Die::Fatal(msg::FSMONITOR_NOT_SUPPORTED.to_owned())),
        "stop" => Err(Die::Fatal(msg::FSMONITOR_NOT_RUNNING.to_owned())),
        "status" => {
            let work_tree = setup
                .work_tree
                .as_deref()
                .unwrap_or(&setup.git_dir)
                .display()
                .to_string();
            println!("{}", msg::fsmonitor_not_watching(&work_tree));
            Ok(1)
        }
        other => Err(Die::Fatal(msg::fsmonitor_unhandled_subcommand(other))),
    }
}
