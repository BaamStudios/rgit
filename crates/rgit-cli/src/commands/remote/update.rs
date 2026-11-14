// SPDX-License-Identifier: MIT
//! `git remote update` and the fetches of `git remote add -f`.
//!
//! `update` behaves like `git fetch --multiple`: every remote is fetched in
//! turn after a `Fetching <name>` line, and `FETCH_HEAD` collects all of
//! them. Without arguments it uses the group `remotes.default`, else every
//! remote without `remote.<name>.skipDefaultUpdate`; a single such remote
//! is fetched like `git fetch <name>`.

use bstr::ByteSlice;

use super::common::Repo;
use crate::messages::remote as msg;
use crate::prelude::*;

/// Run the built-in `fetch` with `argv`; its messages are printed, the exit
/// code returned.
pub fn fetch_one(ctx: &mut Context, argv: &[String]) -> i32 {
    fetch_as(ctx, argv, None)
}

/// [`fetch_one`] with the reflog action `action`.
fn fetch_as(ctx: &mut Context, argv: &[String], action: Option<&str>) -> i32 {
    match crate::commands::fetch::run_with_reflog_action(ctx, argv, action) {
        Ok(code) => code,
        Err(die) => die.emit(),
    }
}

/// The members of group `name` (`remotes.<name>`, whitespace separated),
/// if the group is configured.
fn group(config: &rgit_config::ConfigSet, name: &str) -> Option<Vec<String>> {
    let key = format!("remotes.{name}");
    let mut found = false;
    let mut out = Vec::new();
    for e in config.get_all(key.as_bytes()) {
        found = true;
        if let Some(v) = &e.value {
            out.extend(v.to_str_lossy().split_ascii_whitespace().map(str::to_owned));
        }
    }
    found.then_some(out)
}

pub fn run(ctx: &mut Context, argv: &[String], verbose: bool) -> CmdResult {
    let prune = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_UPDATE,
        vec![Opt::bool('p', "prune", &prune, msg::HELP_UPDATE_PRUNE)],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);

    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    let all_remotes = || -> Result<Vec<String>, Die> {
        let mut out = Vec::new();
        for name in super::remote_names(&repo.config) {
            let r = remotes
                .get(&name, None)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if !r.skip_fetch_all {
                out.push(name.to_string());
            }
        }
        Ok(out)
    };

    let mut names: Vec<String> = Vec::new();
    let mut multiple = true;
    if args.is_empty() {
        match group(&repo.config, "default") {
            Some(members) => names = members,
            None => {
                names = all_remotes()?;
                multiple = names.len() != 1;
            }
        }
    } else {
        for arg in &args {
            if let Some(members) = group(&repo.config, arg) {
                names.extend(members);
            } else if arg == "default" {
                names.extend(all_remotes()?);
            } else if super::remote_names(&repo.config)
                .iter()
                .any(|n| n == arg.as_bytes())
            {
                names.push(arg.clone());
            } else {
                eprintln!("fatal: {}", msg::no_such_group(arg));
                return Ok(1);
            }
        }
    }
    // Like git, only repeated neighbours are dropped.
    names.dedup();

    let mut opts: Vec<String> = Vec::new();
    match prune.get() {
        1 => opts.push("--prune".into()),
        0 => opts.push("--no-prune".into()),
        _ => {}
    }
    if verbose {
        opts.push("-v".into());
    }
    if !multiple {
        // `git fetch --multiple --all` with a single remote fetches it
        // directly; the reflog names that command.
        let mut argv = vec!["fetch".to_owned()];
        argv.extend(opts);
        let action = format!("{} --multiple --all", argv.join(" "));
        argv.extend(names);
        return Ok(if fetch_as(ctx, &argv, Some(&action)) == 0 { 0 } else { 1 });
    }

    // Like `git fetch --multiple`: FETCH_HEAD collects every remote.
    if !names.is_empty() {
        let _ = std::fs::write(repo.setup.git_dir.join("FETCH_HEAD"), "");
    }
    let mut code = 0;
    for name in &names {
        println!("{}", msg::fetching(name));
        let mut argv: Vec<String> = ["fetch", "--append", "--no-auto-gc", "--no-write-commit-graph"]
            .map(String::from)
            .into();
        argv.extend(opts.iter().cloned());
        argv.push(name.clone());
        if fetch_one(ctx, &argv) != 0 {
            eprintln!("error: {}", msg::could_not_fetch(name));
            code = 1;
        }
    }
    Ok(code)
}
