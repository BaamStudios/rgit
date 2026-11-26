// SPDX-License-Identifier: MIT
//! `git rerere` (Documentation/git-rerere.adoc, config/rerere.adoc,
//! config/gc.adoc): without a subcommand one rerere pass; `clear`,
//! `forget <pathspec>...`, `diff`, `status`, `remaining`, `gc`. The work
//! is done by [`rgit_rerere`] through [`crate::rerere`]. Every subcommand
//! does nothing while rerere is off.

use std::io::Write as _;
use std::time::SystemTime;

use rgit_index::Stage;
use rgit_pathspec::{ParseOptions, Pathspec};

use crate::messages::rerere as msg;
use crate::prelude::*;
use crate::repository;
use crate::rerere::{Rerere, flag};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let autoupdate = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![Opt::bool(
            None,
            "rerere-autoupdate",
            &autoupdate,
            msg::HELP_AUTOUPDATE,
        )],
    );
    let args = parser.parse(argv)?.args;
    let usage_error = parser.usage_error();
    drop(parser);
    let sub = args.first().map(String::as_str);
    if !matches!(
        sub,
        None | Some("clear" | "forget" | "diff" | "status" | "remaining" | "gc")
    ) {
        return Err(Die::Output(usage_error));
    }
    let Some(rr) = Rerere::open(ctx)? else {
        return Ok(0);
    };
    match sub {
        None => rr.run(ctx, flag(autoupdate.get()))?,
        Some("clear") => rr.clear()?,
        Some("forget") => forget(ctx, &rr, &args[1..])?,
        Some("status") => {
            let entries = rr.read_merge_rr()?;
            print_paths(&rgit_rerere::status(&entries));
        }
        Some("remaining") => {
            let entries = rr.read_merge_rr()?;
            let mut host = rr.host(ctx)?;
            let conflicts = rgit_rerere::Host::conflicts(&mut host);
            print_paths(&rgit_rerere::remaining(&entries, &conflicts));
        }
        Some("diff") => {
            let entries = rr.read_merge_rr()?;
            let mut host = rr.host(ctx)?;
            let out = rgit_rerere::diff(&rr.cache, &entries, &mut host);
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(&out);
            let _ = o.flush();
        }
        _ => gc(&rr)?,
    }
    Ok(0)
}

fn print_paths(paths: &[Vec<u8>]) {
    let mut o = std::io::stdout().lock();
    for p in paths {
        let _ = o.write_all(p);
        let _ = o.write_all(b"\n");
    }
    let _ = o.flush();
}

/// `rerere forget <pathspec>...`.
fn forget(ctx: &Context, rr: &Rerere, args: &[String]) -> Result<(), Die> {
    if args.is_empty() {
        eprintln!("warning: {}", msg::FORGET_WITHOUT_PATHS);
    }
    let root = rr.repo.work_tree()?;
    let opts = ParseOptions {
        prefix: rr.repo.prefix(),
        root: Some(root.as_os_str().as_encoded_bytes().to_vec()),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec = Pathspec::parse(args, &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    let lock = rr.lock()?;
    let mut entries = rr.read_merge_rr()?;
    let mut host = rr.host(ctx)?;
    // The conflicted paths, and the resolved ones the index can still
    // unresolve.
    let mut paths: Vec<Vec<u8>> = host
        .index
        .entries()
        .iter()
        .filter(|e| e.stage() != Stage::MERGED)
        .map(|e| e.path().to_vec())
        .collect();
    paths.extend(host.index.resolve_undo_records().into_iter().map(|r| r.path));
    paths.sort();
    paths.dedup();
    let mut conflicts = Vec::new();
    for p in paths {
        if !args.is_empty() && pathspec.match_path(&p).is_none() {
            continue;
        }
        if let Some(text) = host.conflict_text(&p) {
            conflicts.push((p, text));
        }
    }
    rgit_rerere::forget(&rr.cache, &mut entries, &mut host, rr.repo.hash, &conflicts)
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    rr.write_merge_rr(lock, &entries)
}

/// `rerere gc` with `gc.rerereResolved` (60 days) and
/// `gc.rerereUnresolved` (15 days): a number of days or a date.
fn gc(rr: &Rerere) -> Result<(), Die> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cutoff = |key: &str, days: u64| -> Result<u64, Die> {
        let value = rr
            .repo
            .config
            .string(key)
            .map_err(|e| Die::Fatal(e.to_string()))?
            .map(|v| v.to_string());
        let Some(v) = value else {
            return Ok(now.saturating_sub(days * 86400));
        };
        if let Ok(n) = v.trim().parse::<i64>() {
            let secs = n.saturating_mul(86400);
            return Ok(if secs >= 0 {
                now.saturating_sub(secs.unsigned_abs())
            } else {
                now.saturating_add(secs.unsigned_abs())
            });
        }
        rgit_date::parse_expiry_date(&v, &rgit_date::DateContext::from_env())
            .map_err(|_| Die::Fatal(format!("Invalid {key}: '{v}'")))
    };
    let resolved = cutoff("gc.rerereresolved", 60)?;
    let unresolved = cutoff("gc.rerereunresolved", 15)?;
    rgit_rerere::gc(&rr.cache, rr.repo.hash, resolved, unresolved);
    Ok(())
}
