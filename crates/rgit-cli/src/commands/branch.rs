// SPDX-License-Identifier: MIT
//! `git branch` (Documentation/git-branch.adoc), first slice: listing
//! (`-a`, `-r`, `--list <pattern>`, `--show-current`), creating
//! `<name> [<start>]` with a reflog entry, and deleting with `-D`.
//!
//! Not yet: `-d` merged check, `-m`/`-c`, `-f`, `-v`, colors, columns,
//! `--format`, `--merged`, `--contains`, tracking setup.

use std::io::Write;

use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::name::check_branch_name;
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Target, Transaction};

use crate::messages::branch as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects, UpdateHooks};
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let remotes = Cell::new(false);
    let all = Cell::new(false);
    let list = Cell::new(false);
    let show_current = Cell::new(false);
    let delete = Cell::new(false);
    let force_delete = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('r', "remotes", &remotes, msg::HELP_REMOTES),
            Opt::flag('a', "all", &all, msg::HELP_ALL),
            Opt::flag('l', "list", &list, msg::HELP_LIST),
            Opt::flag(None, "show-current", &show_current, msg::HELP_SHOW_CURRENT),
            Opt::flag('d', "delete", &delete, msg::HELP_DELETE),
            Opt::flag('D', None, &force_delete, msg::HELP_FORCE_DELETE).noneg(),
        ],
    );
    let args = parser.parse(argv)?.args;

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let mut out = std::io::stdout().lock();

    let head = store.read_raw("HEAD").ok().flatten();
    let current = match &head {
        Some(Target::Symbolic(t)) => t.strip_prefix("refs/heads/").map(str::to_owned),
        _ => None,
    };

    if show_current.get() {
        if let Some(c) = &current {
            writeln!(out, "{c}").map_err(io_err)?;
        }
        return Ok(0);
    }

    if force_delete.get() || delete.get() {
        if !force_delete.get() {
            return Err(Die::Fatal(msg::D_NOT_SUPPORTED.to_owned()));
        }
        if args.is_empty() {
            return Err(Die::Fatal(msg::BRANCH_NAME_REQUIRED.to_owned()));
        }
        let hash = setup.format.object_format;
        let mut code = 0;
        for name in &args {
            let refname = format!("refs/heads/{name}");
            if current.as_deref() == Some(name.as_str()) {
                let path = setup
                    .work_tree
                    .as_deref()
                    .unwrap_or(&setup.git_dir)
                    .display()
                    .to_string();
                eprintln!("error: {}", msg::cannot_delete_checked_out(name, &path));
                code = 1;
                continue;
            }
            let oid = match store.read_raw(&refname) {
                Ok(Some(Target::Object(oid))) => oid,
                _ => {
                    eprintln!("error: {}", msg::not_found(name));
                    code = 1;
                    continue;
                }
            };
            let mut hooks = UpdateHooks::new(&setup, &config, &objects);
            let mut tx = Transaction::new(&store);
            let r = tx
                .push(RefUpdate::delete(refname.as_str(), hash, Some(oid)).no_deref(true))
                .and_then(|()| tx.commit(&mut hooks));
            match r {
                Ok(()) => writeln!(out, "{}", msg::deleted(name, &objects.abbrev(&oid, 7))).map_err(io_err)?,
                Err(e) => {
                    eprintln!("error: {e}");
                    code = 1;
                }
            }
        }
        return Ok(code);
    }

    if !list.get() && !remotes.get() && !all.get() && !args.is_empty() {
        if args.len() > 2 {
            return Err(parser.usage_error().into());
        }
        let name = &args[0];
        if name == "HEAD" || check_branch_name(name.as_bytes()).is_err() {
            eprintln!("fatal: {}", msg::not_valid_branch_name(name));
            if !matches!(config.bool("advice.refsyntax"), Ok(Some(false))) {
                eprint!("{}", msg::HINT_REF_SYNTAX);
            }
            return Ok(128);
        }
        let refname = format!("refs/heads/{name}");
        if matches!(store.read_raw(&refname), Ok(Some(_))) {
            return Err(Die::Fatal(msg::already_exists(name)));
        }
        let start = args.get(1).map(String::as_str).unwrap_or("HEAD");
        let revs = refs_support::Revs::new(ctx, &setup)?;
        let oid = revs
            .resolve(start)
            .and_then(|id| objects.peel(&id).or(Some(id)))
            .ok_or_else(|| Die::Fatal(msg::not_valid_object_name(start)))?;
        let mut hooks = UpdateHooks::new(&setup, &config, &objects);
        let mut tx = Transaction::new(&store);
        tx.push(
            RefUpdate::create(refname.as_str(), oid)
                .message(msg::created_from(start))
                .create_reflog(true),
        )
        .and_then(|()| tx.commit(&mut hooks))
        .map_err(|e| Die::Fatal(e.to_string()))?;
        return Ok(0);
    }

    // Listing.
    let local = !remotes.get() || all.get();
    let remote = remotes.get() || all.get();
    let mut prefixes = Vec::new();
    if local {
        prefixes.push("refs/heads/");
    }
    if remote {
        prefixes.push("refs/remotes/");
    }
    if let (true, Some(Target::Object(oid))) = (local, &head) {
        writeln!(out, "* {}", msg::detached_at(&objects.abbrev(oid, 7))).map_err(io_err)?;
    }
    for prefix in prefixes {
        let items = store
            .iter(&IterOptions::prefix(prefix))
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for item in items {
            let IterItem::Ref(r) = item else { continue };
            let short = &r.name[prefix.len()..];
            let shown = if prefix == "refs/remotes/" && all.get() {
                format!("remotes/{short}")
            } else {
                short.to_owned()
            };
            if !args.is_empty()
                && !args
                    .iter()
                    .any(|p| wildmatch(p.as_bytes(), shown.as_bytes(), WildmatchFlags::default()))
            {
                continue;
            }
            let marker = if prefix == "refs/heads/" && current.as_deref() == Some(short) {
                '*'
            } else {
                ' '
            };
            match &r.target {
                Target::Symbolic(t) => {
                    let t = t
                        .strip_prefix("refs/remotes/")
                        .or_else(|| t.strip_prefix("refs/heads/"))
                        .unwrap_or(t);
                    writeln!(out, "{marker} {shown} -> {t}").map_err(io_err)?;
                }
                Target::Object(_) => writeln!(out, "{marker} {shown}").map_err(io_err)?,
            }
        }
    }
    Ok(0)
}

fn io_err(e: std::io::Error) -> Die {
    Die::Fatal(e.to_string())
}
