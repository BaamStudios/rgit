// SPDX-License-Identifier: MIT
//! `git maintenance run | start | stop | register | unregister | is-needed`
//! (Documentation/git-maintenance.adoc). The work is in
//! [`crate::maintenance`].

use std::io::IsTerminal;

use crate::maintenance::schedule::{self, Scheduler};
use crate::maintenance::tasks::{self, Schedule, Task};
use crate::messages::maintenance as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use rgit_options::CallbackError;

const SUBCOMMANDS: &[&str] = &["run", "start", "stop", "register", "unregister", "is-needed"];

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        msg::USAGE,
        SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)).collect(),
    );
    let parsed = parser.parse(argv)?;
    drop(parser);
    let args = parsed.args;
    match parsed.subcommand.as_deref() {
        Some("run") => run_cmd(ctx, &args),
        Some("start") => start(ctx, &args),
        Some("stop") => stop(ctx, &args),
        Some("register") => register(ctx, &args),
        Some("unregister") => unregister(ctx, &args),
        Some("is-needed") => is_needed(ctx, &args),
        _ => Err(Die::Output(
            Parser::new(msg::USAGE, Vec::new()).error_with_usage("need a subcommand"),
        )),
    }
}

/// `--task=<task>`: append, rejecting unknown and repeated tasks.
fn task_option<'a>(
    tasks: &'a RefCell<Vec<Task>>,
    help: &'a str,
) -> Opt<'a> {
    Opt::callback(
        None,
        "task",
        Some(msg::ARGH_TASK),
        help,
        move |v: Option<&str>, unset: bool| {
            if unset {
                tasks.borrow_mut().clear();
                return Ok(());
            }
            let v = v.unwrap_or_default();
            let Some(t) = Task::from_name(v) else {
                return Err(CallbackError::Error(msg::not_a_task(v)));
            };
            if tasks.borrow().contains(&t) {
                return Err(CallbackError::Error(msg::task_twice(v)));
            }
            tasks.borrow_mut().push(t);
            Ok(())
        },
    )
    .noneg()
}

fn run_cmd(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let auto = Cell::new(false);
    let detach = Cell::new(-1);
    let quiet = Cell::new(-1);
    let schedule: Cell<Option<Schedule>> = Cell::new(None);
    let tasks: RefCell<Vec<Task>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::RUN_USAGE,
        vec![
            Opt::flag(None, "auto", &auto, msg::H_AUTO),
            Opt::bool(None, "detach", &detach, msg::H_DETACH),
            Opt::callback(
                None,
                "schedule",
                Some(msg::ARGH_FREQUENCY),
                msg::H_SCHEDULE,
                |v: Option<&str>, unset: bool| {
                    if unset {
                        schedule.set(None);
                        return Ok(());
                    }
                    let v = v.unwrap_or_default();
                    match Schedule::parse(v) {
                        Some(s) => {
                            schedule.set(Some(s));
                            Ok(())
                        }
                        None => Err(CallbackError::Fatal(msg::unrecognized_schedule(v))),
                    }
                },
            ),
            Opt::bool(None, "quiet", &quiet, msg::H_QUIET),
            task_option(&tasks, msg::H_TASK),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let schedule = schedule.get();
    if auto.get() && schedule.is_some() {
        return Err(Die::Fatal(msg::incompatible("--auto", "--schedule=")));
    }
    let tasks = tasks.into_inner();
    if !tasks.is_empty() && schedule.is_some() {
        return Err(Die::Fatal(msg::incompatible("--task=", "--schedule=")));
    }
    let repo = Repo::open(ctx, Mode::Required)?;
    let opts = tasks::RunOptions {
        auto: auto.get(),
        quiet: match quiet.get() {
            -1 => !std::io::stderr().is_terminal(),
            q => q != 0,
        },
        detach: detach.get() == 1,
    };
    tasks::run(ctx, &repo, &tasks, schedule, &opts)
}

fn is_needed(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let auto = Cell::new(false);
    let tasks: RefCell<Vec<Task>> = RefCell::new(Vec::new());
    let mut parser = Parser::new(
        msg::IS_NEEDED_USAGE,
        vec![
            Opt::flag(None, "auto", &auto, msg::H_AUTO),
            task_option(&tasks, msg::H_TASK_CHECK),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    let needed = tasks::is_needed(&repo, &tasks.into_inner(), auto.get())?;
    Ok(if needed { 0 } else { 1 })
}

fn start(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let scheduler: Cell<Option<Scheduler>> = Cell::new(None);
    let mut parser = Parser::new(
        msg::START_USAGE,
        vec![
            Opt::callback(
                None,
                "scheduler",
                Some(msg::ARGH_SCHEDULER),
                msg::H_SCHEDULER,
                |v: Option<&str>, _| {
                    let v = v.unwrap_or_default();
                    match Scheduler::parse(v) {
                        Ok(s) => {
                            scheduler.set(s);
                            Ok(())
                        }
                        Err(()) => Err(CallbackError::Error(msg::unrecognized_scheduler(v))),
                    }
                },
            )
            .noneg(),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    schedule::start(ctx, &repo, scheduler.get())
}

fn stop(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(msg::STOP_USAGE, Vec::new());
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    schedule::stop(&repo)
}

fn register(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let file: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::REGISTER_USAGE,
        vec![Opt::string(None, "config-file", &file, "file", msg::H_CONFIG_FILE)],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    schedule::register(ctx, &repo, file.borrow().as_deref())
}

fn unregister(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let file: RefCell<Option<String>> = RefCell::new(None);
    let force = Cell::new(false);
    let mut parser = Parser::new(
        msg::UNREGISTER_USAGE,
        vec![
            Opt::string(None, "config-file", &file, "file", msg::H_CONFIG_FILE),
            Opt::flag('f', "force", &force, msg::H_FORCE),
        ],
    );
    let rest = parser.parse(argv)?.args;
    if !rest.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let repo = Repo::open(ctx, Mode::Required)?;
    schedule::unregister(&repo, file.borrow().as_deref(), force.get())
}
