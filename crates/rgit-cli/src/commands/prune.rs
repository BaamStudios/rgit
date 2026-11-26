// SPDX-License-Identifier: MIT
//! `git prune` (Documentation/git-prune.adoc): delete loose objects that
//! nothing reaches, then run `prune-packed`, remove stale `tmp_*` files
//! and drop unreachable entries from `.git/shallow`.
//!
//! Reachable means reachable from the starting points of
//! [`crate::housekeeping::reach`] or from a `<head>` argument. With
//! `--expire`, unreachable objects newer than the date are kept, and so
//! is everything reachable from them (an old blob of a recent tree).

use std::io::Write;

use rgit_hash::ObjectId;

use crate::housekeeping::reach::{self, BadRef, Walk};
use crate::housekeeping::store;
use crate::messages::housekeeping as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let dry_run = Cell::new(0);
    let verbose = Cell::new(0);
    let progress = Cell::new(false);
    let expire: RefCell<Option<String>> = RefCell::new(None);
    let exclude_promisor = Cell::new(false);
    let mut parser = Parser::new(
        msg::PRUNE_USAGE,
        vec![
            Opt::dry_run(&dry_run, msg::PRUNE_H_DRY_RUN),
            Opt::verbose(&verbose, msg::PRUNE_H_VERBOSE),
            Opt::flag(None, "progress", &progress, msg::PRUNE_H_PROGRESS),
            Opt::string(
                None,
                "expire",
                &expire,
                msg::PRUNE_ARGH_EXPIRE,
                msg::PRUNE_H_EXPIRE,
            ),
            Opt::flag(
                None,
                "exclude-promisor-objects",
                &exclude_promisor,
                msg::PRUNE_H_EXCLUDE_PROMISOR,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let dry_run = dry_run.get() != 0;
    let verbose = verbose.get() != 0;
    let expire = match expire.take() {
        None => u64::MAX,
        Some(v) => parse_expire(&v)?,
    };

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    if setup.format.precious_objects {
        return Err(Die::Fatal(msg::PRUNE_PRECIOUS.into()));
    }
    let mut heads = Vec::new();
    if !args.is_empty() {
        let revs = crate::refs_support::Revs::new(ctx, &setup)?;
        for a in &args {
            match revs.resolve(a) {
                Some(id) => heads.push(id),
                None => return Err(Die::Fatal(msg::unrecognized_argument(a))),
            }
        }
    }
    let odb = repo::odb_with_config(&setup, &config)?;
    let kind = setup.format.object_format;
    let objects = rgit_odb::objects_dir(&setup.common_dir);

    let roots = reach::roots(&setup, &odb).map_err(|e| match e {
        BadRef::Unparsable(n) | BadRef::Missing(n) => Die::Fatal(msg::unable_to_parse(&n)),
    })?;
    let common = setup.absolute_common_dir();
    let mut walk = Walk::new(&odb, reach::shallow_commits(&common, kind));
    // A partial clone: the walk stops at promisor objects, which may refer
    // to objects that are not here.
    if crate::promisor::has_remote(&config) {
        walk.seen.extend(odb.promisor_objects().iter().copied());
    }
    let strict: Vec<ObjectId> = roots.refs.iter().map(|(_, id)| *id).chain(heads).collect();
    walk.walk(&strict, false)?;
    walk.walk(&roots.weak, true)?;

    let loose = store::loose_objects(&objects, kind);
    let packs = store::local_packs(&odb, &[]);
    let hooked = crate::housekeeping::recent_objects_hook(&config, &odb)?;
    if !hooked.is_empty() {
        walk.walk(&hooked, true)?;
    }
    if expire != u64::MAX {
        // Unreachable but recent objects, and what they reach, stay.
        let mut recent: Vec<ObjectId> = loose
            .iter()
            .filter(|(id, _, mtime)| *mtime > expire && !walk.seen.contains(id))
            .map(|(id, _, _)| *id)
            .collect();
        for p in packs.iter().filter(|p| p.mtime > expire) {
            recent.extend(p.pack.index().ids().filter(|id| !walk.seen.contains(id)));
        }
        walk.walk(&recent, true)?;
    }
    drop(loose);

    let mut out = std::io::stdout().lock();
    for fan in 0..=255u8 {
        let entries = store::fan_entries(&objects, fan, kind);
        if entries.is_empty() {
            continue;
        }
        for e in entries {
            if let Some(id) = e.id {
                if walk.seen.contains(&id) || e.mtime > expire {
                    continue;
                }
                if dry_run || verbose {
                    let kind_name = odb
                        .loose()
                        .header(&id)
                        .ok()
                        .flatten()
                        .map_or("unknown", |h| h.kind.name());
                    let _ = writeln!(out, "{} {kind_name}", id.to_hex());
                }
                if !dry_run {
                    let _ = std::fs::remove_file(&e.path);
                }
            } else if e.temporary && e.mtime <= expire {
                remove_stale(&mut out, &objects, &e.path, dry_run, verbose);
            }
        }
        if !dry_run {
            let _ = std::fs::remove_dir(objects.join(format!("{fan:02x}")));
        }
    }
    let _ = out.flush();
    drop(out);

    store::prune_packed(&objects, &objects, kind, &packs, dry_run)?;

    let mut out = std::io::stdout().lock();
    for dir in [objects.clone(), objects.join("pack")] {
        for (path, mtime) in store::temporary_files(&dir) {
            if mtime <= expire {
                remove_stale(&mut out, &objects, &path, dry_run, verbose);
            }
        }
    }
    drop(out);

    if !dry_run {
        reach::prune_shallow(&odb, &common, kind);
    }
    Ok(0)
}

fn parse_expire(v: &str) -> Result<u64, Die> {
    rgit_date::parse_expiry_date(v, &rgit_date::DateContext::from_env())
        .map_err(|_| Die::Fatal(msg::malformed_expiration(v)))
}

/// Report and remove a stale temporary file.
fn remove_stale(
    out: &mut dyn Write,
    objects: &std::path::Path,
    path: &std::path::Path,
    dry_run: bool,
    verbose: bool,
) {
    if dry_run || verbose {
        // Shown below the objects directory as the user named it.
        let shown = path
            .strip_prefix(objects)
            .map(|rel| objects.join(rel))
            .unwrap_or_else(|_| path.to_path_buf());
        let _ = writeln!(out, "{}", msg::removing_stale(&shown.display().to_string()));
    }
    if !dry_run {
        let _ = std::fs::remove_file(path);
    }
}
