// SPDX-License-Identifier: MIT
//! `git hook run` and `git hook list` (Documentation/git-hook.adoc,
//! Documentation/config/hook.adoc). Works outside a repository too, with
//! the hooks configured in the system and global configuration.

use std::io::Write;

use rgit_config::{ConfigSet, SystemEnv};
use rgit_hook::{Hooks, Input, RunOptions, State};

use crate::hooks;
use crate::messages::hook as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(
        &[msg::USAGE_RUN, msg::USAGE_LIST],
        vec![Opt::subcommand("run"), Opt::subcommand("list")],
    );
    let parsed = parser.parse(argv)?;
    let setup = crate::repo::setup_gently(ctx)?;
    let config = match &setup {
        Some(s) => crate::repo::load_config(ctx, s)?,
        None => config_outside_repository(ctx),
    };
    let prefix = setup
        .as_ref()
        .and_then(|s| s.prefix.as_ref())
        .map(|p| p.to_string_lossy().into_owned());
    let hooks = match &setup {
        Some(s) => hooks::for_repo(ctx, s, &config)?,
        None => {
            let cwd = std::env::current_dir().map_err(|e| Die::Fatal(e.to_string()))?;
            hooks::outside_repo(ctx, cwd, &config)?
        }
    };
    match parsed.subcommand.as_deref() {
        Some("list") => list(&parsed.args, &hooks),
        _ => run_hooks(&parsed.args, prefix.as_deref(), &hooks),
    }
}

/// The configuration outside a repository: system, global and `-c`.
fn config_outside_repository(ctx: &Context) -> ConfigSet {
    let env = SystemEnv::default();
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let mut set = ConfigSet::new();
    let _ = rgit_config::sources::load_standard(
        opts,
        None,
        &crate::repo::command_line(ctx),
        rgit_config::sources::ScopeFilter::default(),
        &mut set,
    );
    set
}

fn check_name(name: &str, allow_unknown: bool) -> Result<(), Die> {
    if !allow_unknown && !rgit_hook::is_known(name) {
        return Err(Die::Error(rgit_hook::messages::unknown_hook_event(name), 1));
    }
    Ok(())
}

/// `git hook run`.
fn run_hooks(argv: &[String], prefix: Option<&str>, hooks: &Hooks) -> CmdResult {
    let allow_unknown = Cell::new(false);
    let ignore_missing = Cell::new(false);
    let to_stdin = RefCell::new(None);
    let jobs = Cell::new(0);
    let mut parser = Parser::new(
        &[msg::USAGE_RUN],
        vec![
            Opt::flag(None, "allow-unknown-hook-name", &allow_unknown, msg::H_ALLOW_UNKNOWN),
            Opt::flag(None, "ignore-missing", &ignore_missing, msg::H_IGNORE_MISSING),
            Opt::filename(None, "to-stdin", &to_stdin, msg::H_TO_STDIN).argh("path"),
            Opt::integer('j', "jobs", &jobs, msg::H_JOBS),
        ],
    )
    .prefix(prefix)
    .flags(ParseFlags {
        keep_dashdash: true,
        stop_at_non_option: true,
        ..Default::default()
    });
    let args = parser.parse(argv)?.args;
    let Some(name) = args.first() else {
        return Err(parser.usage_error().into());
    };
    let hook_args = match args.get(1).map(String::as_str) {
        None => &[][..],
        Some("--" | "--end-of-options") => &args[2..],
        Some(_) => return Err(parser.usage_error().into()),
    };
    check_name(name, allow_unknown.get())?;
    let path = to_stdin.borrow().clone();
    if let Some(p) = &path {
        if let Err(e) = std::fs::File::open(p) {
            return Err(Die::Fatal(rgit_hook::messages::could_not_open_for_reading(
                p,
                &crate::global::strerror(&e),
            )));
        }
    }
    let mut opts = RunOptions::with_args(hook_args.iter().map(String::as_str));
    if let Some(p) = &path {
        opts.stdin = Input::File(std::path::Path::new(p));
    }
    opts.jobs = match jobs.get() {
        0 => None,
        j => Some(i64::from(j)),
    };
    let outcome = hooks::run(hooks, name, &opts)?;
    if !outcome.found {
        if ignore_missing.get() {
            return Ok(0);
        }
        return Err(Die::Error(rgit_hook::messages::cannot_find_hook(name), 1));
    }
    Ok(outcome.code)
}

/// `git hook list`.
fn list(argv: &[String], hooks: &Hooks) -> CmdResult {
    let z = Cell::new(false);
    let show_scope = Cell::new(false);
    let allow_unknown = Cell::new(false);
    let mut parser = Parser::new(
        &[msg::USAGE_LIST],
        vec![
            Opt::flag('z', None, &z, msg::H_Z).noneg(),
            Opt::flag(None, "show-scope", &show_scope, msg::H_SHOW_SCOPE),
            Opt::flag(None, "allow-unknown-hook-name", &allow_unknown, msg::H_ALLOW_UNKNOWN),
        ],
    );
    let args = parser.parse(argv)?.args;
    let name = match args.as_slice() {
        [n] => n,
        [] => return Err(parser.usage_msg(msg::NO_EVENT_TO_LIST).into()),
        _ => return Err(parser.usage_error().into()),
    };
    check_name(name, allow_unknown.get())?;
    let listed = hooks.list(name).map_err(Die::Fatal)?;
    if listed.is_empty() {
        eprintln!("warning: {}", rgit_hook::messages::no_hooks_found(name));
        return Ok(1);
    }
    let end = if z.get() { '\0' } else { '\n' };
    let mut out = String::new();
    for l in &listed {
        let Some(n) = &l.name else {
            out.push_str(rgit_hook::messages::FROM_HOOKDIR);
            out.push(end);
            continue;
        };
        if show_scope.get() {
            if let Some(s) = l.scope {
                out.push_str(s.as_str());
                out.push('\t');
            }
        }
        match l.state {
            State::Enabled => {}
            State::Disabled => {
                out.push_str(rgit_hook::messages::DISABLED);
                out.push('\t');
            }
            State::EventDisabled => {
                out.push_str(rgit_hook::messages::EVENT_DISABLED);
                out.push('\t');
            }
        }
        out.push_str(n);
        out.push(end);
    }
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(out.as_bytes());
    let _ = o.flush();
    Ok(0)
}
