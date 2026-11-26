// SPDX-License-Identifier: MIT
//! `git repack` (Documentation/git-repack.adoc,
//! Documentation/config/repack.adoc).
//!
//! Without `-a`/`-A` the reachable loose objects are put into a new pack
//! (`-d` then removes their loose copies). With `-a` everything reachable
//! goes into one new pack; with `-d` the old packs are deleted afterwards,
//! and with `-A` the unreachable objects of those packs are first written
//! as loose objects carrying the pack's modification time (the recorded
//! one for objects of a cruft pack), unless older than
//! `--unpack-unreachable`. `-k` keeps unreachable objects in the new pack
//! instead. Packs with a `.keep` file or named by `--keep-pack` are
//! neither repacked nor deleted. The new pack is written by
//! `rgit_pack::write` (packed data is reused; delta search comes with
//! #19); `--window`, `--window-memory` and `--threads` are accepted for
//! it. `update-server-info` runs unless `-n` or `repack.updateServerInfo`
//! is false.
//!
//! `--cruft` is `-a`, followed by `pack-objects --cruft` (run as a child
//! process, like git) for the unreachable objects of the packs being
//! replaced and the loose ones: see `housekeeping::cruft`. Existing cruft
//! packs are rewritten into the new one, except those at least
//! `--combine-cruft-below-size` large. `--max-cruft-size` (else
//! `--max-pack-size`) limits the size of the new cruft packs,
//! `--cruft-expiration` drops old unreachable objects, `--expire-to`
//! keeps the dropped ones in a cruft pack elsewhere, and
//! `repack.cruftWindow`, `repack.cruftWindowMemory`, `repack.cruftDepth`
//! and `repack.cruftThreads` override the delta options for the cruft
//! pack.
//!
//! `--geometric=<factor>` keeps the packs (ordered by object count) in a
//! geometric progression: the smallest packs that break it are rolled up,
//! with the loose objects, into one new pack. Cruft packs and kept packs
//! do not take part; without `-l` the packs of alternates do (their
//! objects are copied, the packs stay).
//!
//! In a repository with a promisor remote (partial clone,
//! Documentation/technical/partial-clone.adoc) promisor packs and other
//! packs are kept apart: `-a`/`-A` put the objects of the promisor packs
//! into one new promisor pack, and the walk for the other pack stops at
//! promisor objects (which may refer to objects that are not here).
//!
//! `--write-midx` writes a multi-pack index of the packs afterwards
//! (`--write-midx=incremental` a new layer of the incremental chain), also
//! after `--geometric`.
//!
//! `-b` (else `repack.writeBitmaps`, on by default in bare repositories)
//! writes a reachability bitmap for the pack of everything reachable, or
//! for the multi-pack index with `--write-midx`. Asked-for bitmaps (not
//! the bare default) make `repack.packKeptObjects` default to true, and a
//! bitmap that cannot be written (objects left out, or the pack split by
//! `--max-pack-size` / `pack.packSizeLimit`) is warned about; the bare
//! default gives up quietly. A pack written again under its old name loses
//! the `.bitmap` and `.rev` files this run does not write.
//!
//! `--filter=<spec>` writes the pack of everything reachable through a
//! `pack-objects --filter` child, like git, and with `-a` the objects
//! left out (those of the replaced packs not in the new pack) into a
//! second pack through `pack-objects --stdin-packs`, in the pack
//! directory or at `--filter-to`. `--drop-filtered` (only with
//! `blob:limit`, in a partial clone) leaves the large blobs of the
//! promisor packs out of the rewritten promisor pack instead; they can be
//! fetched again from the promisor remote. `--dry-run` lists them.
//!
//! `--max-pack-size` (else `pack.packSizeLimit`, at least 1 MiB) splits
//! the pack of what is reachable into several packs.

use std::collections::HashSet;
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_pack::write::ListedObject;

use crate::housekeeping::reach::{self, BadRef, Walk};
use crate::housekeeping::{server_info, store};
use crate::messages::housekeeping as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let all = Cell::new(false);
    let all_loosen = Cell::new(false);
    let cruft = Cell::new(false);
    let cruft_expiration: RefCell<Option<String>> = RefCell::new(None);
    let combine_cruft_below = Cell::new(u64::MAX);
    let max_cruft_size = Cell::new(u64::MAX);
    let max_pack_size = Cell::new(u64::MAX);
    let expire_to: RefCell<Option<String>> = RefCell::new(None);
    let geometric = Cell::new(0i32);
    let delete = Cell::new(false);
    let no_reuse_delta = Cell::new(false);
    let no_reuse_object = Cell::new(false);
    let no_server_info = Cell::new(false);
    let quiet = Cell::new(false);
    let local = Cell::new(false);
    let bitmap = Cell::new(-1);
    let islands = Cell::new(false);
    let unpack_unreachable: RefCell<Option<String>> = RefCell::new(None);
    let keep_unreachable = Cell::new(false);
    let window: RefCell<Option<String>> = RefCell::new(None);
    let window_memory: RefCell<Option<String>> = RefCell::new(None);
    let depth: RefCell<Option<String>> = RefCell::new(None);
    let threads: RefCell<Option<String>> = RefCell::new(None);
    let pack_kept = Cell::new(-1);
    let keep_packs: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let path_walk = Cell::new(false);
    let name_hash_version: RefCell<Option<String>> = RefCell::new(None);
    let write_midx: RefCell<Option<String>> = RefCell::new(None);
    let filters: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let filter_to: RefCell<Option<String>> = RefCell::new(None);
    let drop_filtered = Cell::new(false);
    let dry_run = Cell::new(false);
    let mut parser = Parser::new(
        msg::REPACK_USAGE,
        vec![
            Opt::flag('a', None, &all, msg::REPACK_H_A),
            Opt::flag('A', None, &all_loosen, msg::REPACK_H_BIG_A),
            Opt::flag(None, "cruft", &cruft, msg::REPACK_H_CRUFT),
            Opt::string(
                None,
                "cruft-expiration",
                &cruft_expiration,
                "approxidate",
                msg::REPACK_H_CRUFT_EXPIRATION,
            ),
            Opt::unsigned(
                None,
                "combine-cruft-below-size",
                &combine_cruft_below,
                msg::REPACK_H_COMBINE_CRUFT,
            ),
            Opt::unsigned(None, "max-cruft-size", &max_cruft_size, msg::REPACK_H_MAX_CRUFT),
            Opt::flag('d', None, &delete, msg::REPACK_H_D),
            Opt::flag('f', None, &no_reuse_delta, msg::REPACK_H_F),
            Opt::flag('F', None, &no_reuse_object, msg::REPACK_H_BIG_F),
            Opt::string(
                None,
                "name-hash-version",
                &name_hash_version,
                "n",
                msg::REPACK_H_NAME_HASH,
            ),
            Opt::flag(None, "path-walk", &path_walk, msg::REPACK_H_PATH_WALK),
            Opt::flag('n', None, &no_server_info, msg::REPACK_H_N),
            Opt::flag('q', "quiet", &quiet, msg::REPACK_H_QUIET),
            Opt::flag('l', "local", &local, msg::REPACK_H_LOCAL),
            Opt::bool('b', "write-bitmap-index", &bitmap, msg::REPACK_H_BITMAP),
            Opt::flag('i', "delta-islands", &islands, msg::REPACK_H_ISLANDS),
            Opt::string(
                None,
                "unpack-unreachable",
                &unpack_unreachable,
                "approxidate",
                msg::REPACK_H_UNPACK_UNREACHABLE,
            ),
            Opt::flag('k', "keep-unreachable", &keep_unreachable, msg::REPACK_H_KEEP_UNREACHABLE),
            Opt::string(None, "window", &window, "n", msg::REPACK_H_WINDOW),
            Opt::string(
                None,
                "window-memory",
                &window_memory,
                "bytes",
                msg::REPACK_H_WINDOW_MEMORY,
            ),
            Opt::string(None, "depth", &depth, "n", msg::REPACK_H_DEPTH),
            Opt::string(None, "threads", &threads, "n", msg::REPACK_H_THREADS),
            Opt::unsigned(None, "max-pack-size", &max_pack_size, msg::REPACK_H_MAX_PACK_SIZE),
            Opt::callback(
                None,
                "filter",
                Some("args"),
                msg::REPACK_H_FILTER,
                |v: Option<&str>, unset: bool| -> Result<(), CallbackError> {
                    match (unset, v) {
                        (false, Some(v)) => filters.borrow_mut().push(v.to_owned()),
                        _ => filters.borrow_mut().clear(),
                    }
                    Ok(())
                },
            ),
            Opt::bool(None, "pack-kept-objects", &pack_kept, msg::REPACK_H_PACK_KEPT),
            Opt::string_list(None, "keep-pack", &keep_packs, "name", msg::REPACK_H_KEEP_PACK),
            Opt::integer('g', "geometric", &geometric, msg::REPACK_H_GEOMETRIC),
            Opt::callback(
                None,
                "write-midx",
                Some("mode"),
                msg::REPACK_H_WRITE_MIDX,
                |v: Option<&str>, unset: bool| -> Result<(), CallbackError> {
                    *write_midx.borrow_mut() = (!unset).then(|| v.unwrap_or("default").to_owned());
                    Ok(())
                },
            )
            .optional(None),
            // `-m` (no argument) is `--write-midx`; `-h` does not list it.
            Opt::callback('m', None, None, msg::REPACK_H_WRITE_MIDX, |_, _| {
                *write_midx.borrow_mut() = Some("default".to_owned());
                Ok(())
            })
            .noarg()
            .hidden(),
            Opt::string(None, "expire-to", &expire_to, "dir", msg::REPACK_H_EXPIRE_TO),
            Opt::string(None, "filter-to", &filter_to, "dir", msg::REPACK_H_FILTER_TO),
            Opt::flag(None, "drop-filtered", &drop_filtered, msg::REPACK_H_DROP_FILTERED),
            Opt::flag(None, "dry-run", &dry_run, msg::REPACK_H_DRY_RUN),
        ],
    );
    let _args = parser.parse(argv)?.args;
    drop(parser);
    let filters = filters.take();
    let filter_to = filter_to.take();
    let drop_filtered = drop_filtered.get();
    let drop_limit = check_filter_options(
        &filters,
        filter_to.is_some(),
        drop_filtered,
        dry_run.get(),
        all.get() || all_loosen.get() || cruft.get(),
        bitmap.get() == 1,
    )?;
    if keep_unreachable.get() && cruft.get() {
        return Err(Die::Fatal(msg::REPACK_KEEP_AND_CRUFT.into()));
    }
    if geometric.get() != 0 && (all.get() || all_loosen.get() || cruft.get()) {
        return Err(Die::Fatal(msg::REPACK_GEOMETRIC_AND_ALL.into()));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let int_config = |key: &str| config.int(key).map_err(|e| Die::Fatal(e.to_string()));
    let midx_split_factor = int_config("repack.midxsplitfactor")?.unwrap_or(2);
    if midx_split_factor < 2 {
        return Err(Die::Fatal(msg::midx_bad_split_factor(midx_split_factor)));
    }
    let midx_new_layer_threshold = int_config("repack.midxnewlayerthreshold")?.unwrap_or(8);
    if midx_new_layer_threshold < 1 {
        return Err(Die::Fatal(msg::midx_bad_layer_threshold(
            midx_new_layer_threshold,
        )));
    }
    if drop_filtered && !crate::promisor::has_remote(&config) {
        return Err(Die::Fatal(msg::REPACK_DROP_NEEDS_PROMISOR.into()));
    }
    let cruft = cruft.get();
    let all_loosen = all_loosen.get() && !cruft;
    let pack_everything = all.get() || all_loosen || cruft;
    // `-b`, else `repack.writeBitmaps` (`pack.writeBitmaps`), else true in
    // bare repositories.
    let configured_bitmaps = config
        .bool("repack.writebitmaps")
        .ok()
        .flatten()
        .or_else(|| config.bool("pack.writebitmaps").ok().flatten());
    let mut write_bitmaps = match bitmap.get() {
        -1 => configured_bitmaps
            .unwrap_or_else(|| setup.is_bare() && write_midx.borrow().is_none()),
        v => v > 0,
    };
    // Asked for by -b or the configuration, not just the default of a bare
    // repository: kept objects are packed too, and a bitmap that cannot be
    // written is warned about.
    let explicit_bitmaps = match bitmap.get() {
        -1 => configured_bitmaps == Some(true),
        v => v > 0,
    };
    // A bitmap setting from the configuration is quietly off with
    // --drop-filtered (an explicit -b was refused above).
    if drop_filtered {
        write_bitmaps = false;
    }
    if write_bitmaps && !pack_everything && write_midx.borrow().is_none() {
        if bitmap.get() == -1 && setup.is_bare() {
            write_bitmaps = false;
        } else {
            return Err(Die::Fatal(msg::REPACK_INCREMENTAL_BITMAP.into()));
        }
    }
    let delete = delete.get() || (drop_filtered && !dry_run.get());
    if delete && setup.format.precious_objects {
        return Err(Die::Fatal(msg::REPACK_PRECIOUS.into()));
    }
    let date_ctx = rgit_date::DateContext::from_env();
    let unpack_expire = match unpack_unreachable.take() {
        Some(v) => Some(
            rgit_date::parse_expiry_date(&v, &date_ctx)
                .map_err(|_| Die::Fatal(msg::malformed_expiration(&v)))?,
        ),
        None => None,
    };
    let mut opts = store::PackOptions::from_config(&config)?;
    opts.write.reuse_delta = !no_reuse_delta.get() && !no_reuse_object.get();
    opts.write.reuse_object = !no_reuse_object.get();
    if let Some(d) = depth.borrow().as_deref() {
        let d: i64 = d.trim().parse().unwrap_or(50);
        opts.write.max_depth = d.clamp(0, i64::from(rgit_pack::write::MAX_DEPTH)) as u32;
    }
    // `repack.packKeptObjects` defaults to true when bitmaps are asked for
    // (the default bitmaps of a bare repository leave kept packs alone).
    let pack_kept = match pack_kept.get() {
        -1 => config
            .bool("repack.packkeptobjects")
            .ok()
            .flatten()
            .unwrap_or(explicit_bitmaps && write_bitmaps && write_midx.borrow().is_none()),
        v => v > 0,
    };
    // `--max-pack-size`, else `pack.packSizeLimit`; at least 1 MiB.
    let size_limit = match max_pack_size.get() {
        u64::MAX => config
            .int64("pack.packsizelimit")
            .map_err(|e| Die::Fatal(e.to_string()))?
            .filter(|&v| v > 0)
            .map(|v| v as u64),
        0 => None,
        v => Some(v),
    }
    .map(|v| v.max(1 << 20));
    let keep_names = keep_packs.take();

    let odb = repo::odb_with_config(&setup, &config)?;
    let kind = setup.format.object_format;
    let objects_dir = rgit_odb::objects_dir(&setup.common_dir);
    let pack_dir = objects_dir.join("pack");
    let common = setup.absolute_common_dir();

    if geometric.get() != 0 {
        let g = Geometric {
            factor: geometric.get().max(1) as u64,
            local: local.get(),
            pack_kept,
            delete,
            quiet: quiet.get(),
        };
        g.run(&odb, &pack_dir, &objects_dir, &keep_names, &opts)?;
        if delete {
            store::remove_pack_garbage(&pack_dir);
            let packs_now = store::local_packs(&odb, &keep_names);
            store::prune_packed(&objects_dir, &objects_dir, kind, &packs_now, false)?;
        }
        write_midx_after(
            ctx,
            &setup,
            &config,
            write_midx.take(),
            &pack_dir,
            write_bitmaps,
            quiet.get(),
        )?;
        update_server_info(&setup, &config, &odb, &keep_names, no_server_info.get(), &opts);
        return Ok(0);
    }

    let packs = store::local_packs(&odb, &keep_names);
    let dropped: HashSet<ObjectId> = match drop_limit {
        Some(limit) => {
            let candidates = drop_candidates(&odb, &packs, limit);
            if !setup.is_bare() {
                check_drop_safe(&setup, &candidates)?;
            }
            if dry_run.get() {
                let mut out = std::io::stdout().lock();
                for id in &candidates {
                    let _ = writeln!(out, "{}", id.to_hex());
                }
                return Ok(0);
            }
            candidates.into_iter().collect()
        }
        None => HashSet::new(),
    };
    let kept: Vec<&store::LocalPack> = packs.iter().filter(|p| p.keep).collect();
    let in_kept = |id: &ObjectId| kept.iter().any(|p| p.pack.contains(id));
    let in_local_pack = |id: &ObjectId| packs.iter().any(|p| p.pack.contains(id));
    let loose: Vec<(ObjectId, std::path::PathBuf, u64)> =
        store::loose_objects(&objects_dir, kind);
    let loose_ids: HashSet<ObjectId> = loose.iter().map(|(id, _, _)| *id).collect();
    let is_local = |id: &ObjectId| loose_ids.contains(id) || in_local_pack(id);

    // What is reachable. Promisor objects are left to the promisor packs.
    let partial = crate::promisor::has_remote(&config);
    let mut walk = Walk::new(&odb, reach::shallow_commits(&common, kind));
    if partial {
        walk.seen.extend(odb.promisor_objects().iter().copied());
    }
    let needs_walk = pack_everything || !loose_ids.is_empty();
    if needs_walk {
        let roots = reach::roots(&setup, &odb).map_err(|e| match e {
            BadRef::Unparsable(n) | BadRef::Missing(n) => Die::Fatal(msg::bad_object(&n)),
        })?;
        let strict: Vec<ObjectId> = roots.refs.iter().map(|(_, id)| *id).collect();
        walk.walk(&strict, false)?;
        walk.walk(&roots.weak, true)?;
    }

    // What goes into the new pack.
    let mut chosen: Vec<ListedObject> = walk
        .listed
        .iter()
        .filter(|o| pack_kept || !in_kept(&o.id))
        .filter(|o| !local.get() || is_local(&o.id))
        .filter(|o| pack_everything || (loose_ids.contains(&o.id) && !in_local_pack(&o.id)))
        .copied()
        .collect();
    if pack_everything && keep_unreachable.get() {
        let mut extra: Vec<ObjectId> = Vec::new();
        for p in packs.iter().filter(|p| !p.keep) {
            extra.extend(p.pack.index().ids().filter(|id| !walk.seen.contains(id)));
        }
        extra.extend(loose_ids.iter().filter(|id| !walk.seen.contains(id)));
        extra.sort();
        extra.dedup();
        let described = rgit_pack::write::describe(&odb, kind, &extra)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        chosen.extend(described.into_iter().filter(|o| pack_kept || !in_kept(&o.id)));
    }
    // The objects of the promisor packs go into one promisor pack.
    let mut promisor_name = None;
    if partial && pack_everything {
        let mut ids: Vec<ObjectId> = packs
            .iter()
            .filter(|p| !p.keep && p.pack.is_promisor())
            .flat_map(|p| p.pack.index().ids().collect::<Vec<_>>())
            .filter(|id| !dropped.contains(id))
            .collect();
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            let described = rgit_pack::write::describe(&odb, kind, &ids)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            let ordered = rgit_pack::write::pack_order(&described);
            if let Some(c) = store::write_pack(&odb, &pack_dir, &ordered, &opts)? {
                let name = format!("pack-{}", c.to_hex());
                let marker = pack_dir.join(format!("{name}.promisor"));
                if !marker.exists() {
                    std::fs::write(&marker, b"")
                        .map_err(|e| Die::Fatal(format!("{}: {e}", marker.display())))?;
                }
                promisor_name = Some(format!("{name}.pack"));
            }
        }
    }
    if partial && write_bitmaps && write_midx.borrow().is_none() && pack_everything {
        check_closure(&odb, &chosen)?;
    }
    let ordered = rgit_pack::write::pack_order(&chosen);
    // With --filter, pack-objects writes the pack(s) (and its bitmap).
    let filtered = !filters.is_empty() && !drop_filtered;
    let new_packs = if filtered {
        Vec::new()
    } else {
        store::write_packs(&odb, &pack_dir, &ordered, &opts, size_limit)?
    };
    let mut filter_names: Vec<String> = Vec::new();
    if filtered {
        let job = FilterJob {
            ctx,
            setup: &setup,
            config: &config,
            local: local.get(),
            quiet: quiet.get(),
            pack_kept,
            window: window.borrow().clone(),
            window_memory: window_memory.borrow().clone(),
            depth: depth.borrow().clone(),
            threads: threads.borrow().clone(),
            max_pack_size: (max_pack_size.get() != u64::MAX).then_some(max_pack_size.get()),
        };
        let mut keep: Vec<String> = keep_names.clone();
        if partial {
            // Promisor objects stay in the promisor pack.
            keep.extend(
                store::local_packs(&odb, &keep_names)
                    .iter()
                    .filter(|p| p.pack.is_promisor())
                    .map(|p| p.file_name.clone()),
            );
        }
        let bitmap_arg = (write_bitmaps && pack_everything && write_midx.borrow().is_none())
            .then_some(bitmap.get() == 1);
        let hashes = job.write_reachable(
            &pack_dir.join("pack"),
            pack_everything,
            &keep,
            bitmap_arg,
            &filters,
        )?;
        filter_names = hashes.iter().map(|h| format!("pack-{h}.pack")).collect();
    }
    // A pack that leaves out reachable objects (kept or non-local ones)
    // gets no bitmap, nor do packs split by the size limit; git disables it
    // quietly unless bitmaps were asked for.
    let complete = chosen.len() >= walk.listed.len();
    let pack_bitmap = !filtered && write_bitmaps && write_midx.borrow().is_none();
    if pack_bitmap && explicit_bitmaps {
        if !complete {
            eprintln!(
                "warning: {}",
                crate::messages::pack_objects::BITMAP_DISABLED_NOT_PACKED
            );
        } else if new_packs.len() > 1 {
            eprintln!("warning: {}", crate::messages::pack_objects::BITMAP_DISABLED_SPLIT);
        }
    }
    let mut bitmap_written = false;
    if let [sum] = new_packs.as_slice()
        && pack_bitmap
        && complete
    {
        bitmap_written = true;
        let walk_commits: Vec<ObjectId> = chosen
            .iter()
            .filter(|o| o.kind == rgit_object::ObjectKind::Commit)
            .map(|o| o.id)
            .collect();
        let idx = pack_dir.join(format!("pack-{}.idx", sum.to_hex()));
        crate::bitmaps::write_for_pack(
            &setup,
            &config,
            &odb,
            &idx,
            &walk_commits,
            !quiet.get() && rgit_progress::stderr_is_terminal(),
        )?;
    }
    // A pack written again under its old name keeps only the files this run
    // writes: a `.bitmap` or `.rev` from before goes.
    for sum in &new_packs {
        let base = pack_dir.join(format!("pack-{}", sum.to_hex()));
        if !bitmap_written {
            let _ = std::fs::remove_file(base.with_extension("bitmap"));
        }
        if !opts.rev {
            let _ = std::fs::remove_file(base.with_extension("rev"));
        }
    }
    if new_packs.is_empty() && filter_names.is_empty() && !quiet.get() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", msg::REPACK_NOTHING_NEW);
    }
    let mut new_names: Vec<String> = new_packs
        .iter()
        .map(|c| format!("pack-{}.pack", c.to_hex()))
        .collect();
    new_names.extend(promisor_name);
    new_names.extend(filter_names);
    if filtered && pack_everything {
        // What the filter left out of the replaced packs goes into a pack
        // of its own.
        let mut input = String::new();
        for p in &packs {
            if new_names.contains(&p.file_name) {
                continue;
            }
            if (p.keep && !pack_kept) || p.pack.is_promisor() {
                input.push('^');
            }
            input.push_str(&p.file_name);
            input.push('\n');
        }
        for n in &new_names {
            input.push('^');
            input.push_str(n);
            input.push('\n');
        }
        let base = match &filter_to {
            Some(dir) => std::path::PathBuf::from(dir),
            None => pack_dir.join("pack"),
        };
        let job = FilterJob {
            ctx,
            setup: &setup,
            config: &config,
            local: local.get(),
            quiet: quiet.get(),
            pack_kept,
            window: window.borrow().clone(),
            window_memory: window_memory.borrow().clone(),
            depth: depth.borrow().clone(),
            threads: threads.borrow().clone(),
            max_pack_size: (max_pack_size.get() != u64::MAX).then_some(max_pack_size.get()),
        };
        let hashes = job.write_left_out(&base, &input)?;
        if filter_to.is_none() {
            new_names.extend(hashes.iter().map(|h| format!("pack-{h}.pack")));
        }
    }
    let new_ids: HashSet<ObjectId> = ordered.iter().map(|o| o.id).collect();

    // The packs to replace: all but the kept ones (and, with
    // --combine-cruft-below-size, the large cruft packs).
    let mut replaced: Vec<&store::LocalPack> = packs
        .iter()
        .filter(|p| !p.keep && !new_names.contains(&p.file_name))
        .collect();
    if cruft {
        let size = |p: &store::LocalPack| std::fs::metadata(p.pack.path()).map_or(0, |m| m.len());
        let combine = combine_cruft_below.get();
        replaced.retain(|p| !(combine != u64::MAX && p.pack.is_cruft() && size(p) >= combine));
        let job = CruftJob {
            ctx,
            setup: &setup,
            config: &config,
            local: local.get(),
            quiet: quiet.get(),
            window: window.borrow().clone(),
            window_memory: window_memory.borrow().clone(),
            depth: depth.borrow().clone(),
            threads: threads.borrow().clone(),
            max_size: [max_cruft_size.get(), max_pack_size.get()]
                .into_iter()
                .find(|&v| v != u64::MAX),
        };
        let fresh = store::local_packs(&odb, &keep_names);
        let replaced_names: Vec<&str> = replaced.iter().map(|p| p.file_name.as_str()).collect();
        let written = job.write(
            &fresh,
            &replaced_names,
            &pack_dir.join("pack"),
            cruft_expiration.borrow().as_deref(),
        )?;
        new_names.extend(written.iter().map(|c| format!("pack-{c}.pack")));
        if let (Some(dir), true) = (expire_to.borrow().as_deref(), delete) {
            // The objects that the expiration dropped: everything of the
            // replaced packs that no staying pack has.
            let fresh = store::local_packs(&odb, &keep_names);
            let replaced_names: Vec<&str> = replaced
                .iter()
                .map(|p| p.file_name.as_str())
                .filter(|n| !new_names.iter().any(|m| m == n))
                .collect();
            job.write(&fresh, &replaced_names, std::path::Path::new(dir), None)?;
        }
        replaced.retain(|p| !new_names.contains(&p.file_name));
    }

    if delete {
        if pack_everything {
            if all_loosen && !keep_unreachable.get() {
                let hooked = crate::housekeeping::recent_objects_hook(&config, &odb)?;
                let rescued =
                    rescue_recent(&odb, &walk, &packs, &loose, unpack_expire, &hooked, &common)?;
                let mut loosened = 0usize;
                for p in &replaced {
                    let old_pack = unpack_expire.is_some_and(|t| p.mtime < t);
                    let cruft_pack = p.pack.is_cruft();
                    for id in p.pack.index().ids() {
                        let mtime = if cruft_pack {
                            p.pack.object_mtime(&id).map_or(p.mtime, u64::from)
                        } else {
                            p.mtime
                        };
                        let old = if cruft_pack {
                            unpack_expire.is_some_and(|t| mtime < t)
                        } else {
                            old_pack
                        };
                        if old && !rescued.contains(&id) {
                            continue;
                        }
                        if walk.seen.contains(&id)
                            || new_ids.contains(&id)
                            || in_kept(&id)
                            || loose_ids.contains(&id)
                            || in_alternate(&odb, &id)
                        {
                            continue;
                        }
                        store::loosen(&odb, &id, mtime)?;
                        loosened += 1;
                    }
                }
                // As git's pack-objects child reports it.
                rgit_trace::trace2::data(
                    "pack-objects",
                    "loosen_unused_packed_objects/loosened",
                    &loosened.to_string(),
                    true,
                );
            }
            // The MIDX goes when it lists a pack that is deleted.
            let mut midx_errors = Vec::new();
            let midx_lists_old = rgit_pack::midx::MidxChain::open(&pack_dir, kind, &mut midx_errors)
                .ok()
                .flatten()
                .is_none_or(|m| replaced.iter().any(|p| m.contains_pack(&p.file_name)));
            if !replaced.is_empty() && midx_lists_old {
                store::remove_multi_pack_index(&pack_dir);
            }
            for p in &replaced {
                store::delete_pack(&p.base());
            }
            remove_stale_tmp_packs(&pack_dir);
        }
        store::remove_pack_garbage(&pack_dir);
        let packs_now = store::local_packs(&odb, &keep_names);
        store::prune_packed(&objects_dir, &objects_dir, kind, &packs_now, false)?;
        if pack_everything {
            // Commits dropped with the old packs leave `.git/shallow`.
            let fresh = repo::odb_with_config(&setup, &config)?;
            reach::prune_shallow(&fresh, &common, kind);
        }
    }

    write_midx_after(
        ctx,
        &setup,
        &config,
        write_midx.take(),
        &pack_dir,
        write_bitmaps,
        quiet.get(),
    )?;
    update_server_info(&setup, &config, &odb, &keep_names, no_server_info.get(), &opts);
    Ok(0)
}

/// `--write-midx[=<mode>]`: write the multi-pack index of the packs left,
/// with its bitmap when bitmaps are written.
fn write_midx_after(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    config: &rgit_config::ConfigSet,
    mode: Option<String>,
    pack_dir: &std::path::Path,
    bitmap: bool,
    quiet: bool,
) -> Result<(), Die> {
    let Some(mode) = mode else {
        return Ok(());
    };
    let kind = setup.format.object_format;
    // Without packs there is nothing to index: git's repack says nothing
    // then (`multi-pack-index write` itself reports an error).
    if rgit_pack::PackSet::open(pack_dir, kind).packs().is_empty() {
        return Ok(());
    }
    let opts = rgit_pack::midx::write::WriteOptions {
        version: match config.int("midx.version").ok().flatten() {
            Some(2) => 2,
            _ => 1,
        },
        bitmap,
        incremental: mode == "incremental",
        progress: !quiet && rgit_progress::stderr_is_terminal(),
        // A new incremental layer leaves out kept packs.
        only: (mode == "incremental")
            .then(|| {
                rgit_pack::midx::write::idx_names(pack_dir)
                    .into_iter()
                    .filter(|n| !pack_dir.join(n).with_extension("keep").exists())
                    .collect()
            }),
        ..Default::default()
    };
    let display = pack_dir.display().to_string();
    let hashes = if opts.bitmap && !opts.incremental {
        crate::bitmaps::existing_name_hashes(pack_dir, kind)
    } else {
        Default::default()
    };
    let outcome =
        rgit_pack::midx::write::write(pack_dir, &display, kind, &opts).map_err(|e| match e {
            rgit_pack::midx::write::WriteError::Error(m) => Die::Error(m, 255),
            rgit_pack::midx::write::WriteError::Fatal(m) => Die::Fatal(m),
        })?;
    if opts.bitmap && !opts.incremental && outcome.written.is_some() {
        crate::bitmaps::write_for_midx(ctx, setup, config, pack_dir, &hashes, None)?;
    }
    Ok(())
}

/// `update-server-info` unless `-n` or `repack.updateServerInfo=false`.
fn update_server_info(
    setup: &rgit_repo::Setup,
    config: &rgit_config::ConfigSet,
    odb: &rgit_odb::ObjectDatabase,
    keep_names: &[String],
    no_server_info: bool,
    opts: &store::PackOptions,
) {
    let update_info = !no_server_info
        && config
            .bool("repack.updateserverinfo")
            .ok()
            .flatten()
            .unwrap_or(true);
    if update_info {
        let packs_now = store::local_packs(odb, keep_names);
        let _ = server_info::update(setup, odb, &packs_now, opts.shared, false);
    }
}

/// Check the options of `--filter`, `--filter-to`, `--drop-filtered`
/// and `--dry-run`, in git's order. Returns the size limit of
/// `--drop-filtered`.
fn check_filter_options(
    filters: &[String],
    filter_to: bool,
    drop: bool,
    dry_run: bool,
    all: bool,
    explicit_bitmap: bool,
) -> Result<Option<u64>, Die> {
    let fatal = |m: &str| Err(Die::Fatal(m.to_owned()));
    if drop && filters.is_empty() {
        return fatal(msg::REPACK_DROP_NEEDS_FILTER);
    }
    if dry_run && !drop {
        return fatal(msg::REPACK_DRY_RUN_NEEDS_DROP);
    }
    if drop && filter_to {
        return fatal(msg::REPACK_DROP_AND_FILTER_TO);
    }
    if drop && !all {
        return fatal(msg::REPACK_DROP_NEEDS_ALL);
    }
    if drop && explicit_bitmap {
        return fatal(msg::REPACK_DROP_AND_BITMAP);
    }
    if filter_to && filters.is_empty() {
        return fatal(msg::REPACK_FILTER_TO_NEEDS_FILTER);
    }
    let parsed = crate::promisor::parse_filters(filters)?;
    if !drop {
        return Ok(None);
    }
    match parsed {
        Some((rgit_pack::write::ObjectFilter::BlobLimit(n), _)) => Ok(Some(n)),
        _ => fatal(msg::REPACK_DROP_ONLY_BLOB_LIMIT),
    }
}

/// The objects `--drop-filtered` drops: the blobs of the promisor packs
/// of at least `limit` bytes, in pack order.
fn drop_candidates(
    odb: &rgit_odb::ObjectDatabase,
    packs: &[store::LocalPack],
    limit: u64,
) -> Vec<ObjectId> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for p in packs.iter().filter(|p| !p.keep && p.pack.is_promisor()) {
        let index = p.pack.index();
        let mut by_offset: Vec<(u64, u32)> = (0..index.len())
            .filter_map(|pos| index.offset_at(pos).ok().map(|off| (off, pos)))
            .collect();
        by_offset.sort_unstable();
        for (_, pos) in by_offset {
            let id = index.id_at(pos);
            let Ok(Some(h)) = odb.header(&id) else {
                continue;
            };
            if h.kind == rgit_object::ObjectKind::Blob && h.size >= limit && seen.insert(id) {
                out.push(id);
            }
        }
    }
    out
}

/// In a repository with a worktree, `--drop-filtered` refuses to run
/// while an operation is in progress, and to drop a blob the index names.
fn check_drop_safe(setup: &rgit_repo::Setup, candidates: &[ObjectId]) -> Result<(), Die> {
    let dir = &setup.git_dir;
    let busy = dir.join("MERGE_HEAD").is_file()
        || dir.join("rebase-apply").is_dir()
        || dir.join("rebase-merge").is_dir()
        || dir.join("CHERRY_PICK_HEAD").is_file()
        || dir.join("REVERT_HEAD").is_file()
        || dir.join("BISECT_LOG").is_file();
    if busy {
        return Err(Die::Fatal(msg::REPACK_DROP_IN_PROGRESS.into()));
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let wanted: HashSet<&ObjectId> = candidates.iter().collect();
    let index = rgit_index::Index::read_file(
        rgit_index::index_path(dir),
        setup.format.object_format,
        &rgit_index::ReadOptions::default(),
    );
    if let Ok(index) = index
        && let Some(e) = index.entries().iter().find(|e| wanted.contains(&e.id))
    {
        return Err(Die::Fatal(msg::repack_drop_in_index(
            &String::from_utf8_lossy(e.path()),
            &e.id.to_hex(),
        )));
    }
    Ok(())
}

/// The `pack-objects` children of `--filter`.
struct FilterJob<'a> {
    ctx: &'a Context,
    setup: &'a rgit_repo::Setup,
    config: &'a rgit_config::ConfigSet,
    local: bool,
    quiet: bool,
    pack_kept: bool,
    window: Option<String>,
    window_memory: Option<String>,
    depth: Option<String>,
    threads: Option<String>,
    max_pack_size: Option<u64>,
}

impl FilterJob<'_> {
    /// The options both children get, up to the base name.
    fn common(&self, base: &std::path::Path) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        if let Some(n) = self.max_pack_size {
            args.push(format!("--max-pack-size={n}"));
        }
        if self.local {
            args.push("--local".into());
        }
        if self.quiet {
            args.push("--quiet".into());
        }
        if self
            .config
            .bool("repack.usedeltabaseoffset")
            .ok()
            .flatten()
            .unwrap_or(true)
        {
            args.push("--delta-base-offset".into());
        }
        if !self.pack_kept {
            args.push("--honor-pack-keep".into());
        }
        for (opt, given) in [
            ("window", &self.window),
            ("window-memory", &self.window_memory),
            ("depth", &self.depth),
            ("threads", &self.threads),
        ] {
            if let Some(v) = given {
                args.push(format!("--{opt}={v}"));
            }
        }
        args.push(base.display().to_string());
        args
    }

    /// The pack of what is reachable, filtered. `bitmap`: `Some(true)`
    /// for `-b`, `Some(false)` for a bitmap by configuration.
    fn write_reachable(
        &self,
        base: &std::path::Path,
        all: bool,
        keep: &[String],
        bitmap: Option<bool>,
        filters: &[String],
    ) -> Result<Vec<String>, Die> {
        let mut args = self.common(base);
        for a in ["--keep-true-parents", "--non-empty", "--all", "--reflog", "--indexed-objects"] {
            args.push(a.into());
        }
        if !all {
            args.push("--unpacked".into());
            args.push("--incremental".into());
        }
        for k in keep {
            args.push(format!("--keep-pack={k}"));
        }
        match bitmap {
            Some(true) => args.push("--write-bitmap-index".into()),
            Some(false) => args.push("--write-bitmap-index-quiet".into()),
            None => {}
        }
        for f in filters {
            args.push(format!("--filter={f}"));
        }
        crate::housekeeping::run_pack_objects(self.ctx, self.setup, &args, "")
    }

    /// The pack of the objects of the packs named in `input` (the
    /// `--stdin-packs` list) that the filtered pack left out.
    fn write_left_out(&self, base: &std::path::Path, input: &str) -> Result<Vec<String>, Die> {
        let mut args = self.common(base);
        args.push("--stdin-packs".into());
        crate::housekeeping::run_pack_objects(self.ctx, self.setup, &args, input)
    }
}

/// Writing cruft packs through `pack-objects --cruft`.
struct CruftJob<'a> {
    ctx: &'a Context,
    setup: &'a rgit_repo::Setup,
    config: &'a rgit_config::ConfigSet,
    local: bool,
    quiet: bool,
    window: Option<String>,
    window_memory: Option<String>,
    depth: Option<String>,
    threads: Option<String>,
    max_size: Option<u64>,
}

impl CruftJob<'_> {
    /// Write the cruft pack(s) for `packs` of which `replaced` are about
    /// to go, to `<base>-<hash>.pack`. Returns the new packs' hashes.
    fn write(
        &self,
        packs: &[store::LocalPack],
        replaced: &[&str],
        base: &std::path::Path,
        expiration: Option<&str>,
    ) -> Result<Vec<String>, Die> {
        let mut args: Vec<String> = Vec::new();
        if let Some(n) = self.max_size {
            args.push(format!("--max-pack-size={n}"));
        }
        if self.local {
            args.push("--local".into());
        }
        if self
            .config
            .bool("repack.usedeltabaseoffset")
            .ok()
            .flatten()
            .unwrap_or(true)
        {
            args.push("--delta-base-offset".into());
        }
        args.push("--honor-pack-keep".into());
        // repack.cruft* override the command line, which overrides pack.*.
        let pick = |key: &str, given: &Option<String>| -> Option<String> {
            self.config
                .string(key)
                .ok()
                .flatten()
                .map(|v| v.to_string())
                .or_else(|| given.clone())
        };
        for (opt, key, given) in [
            ("window", "repack.cruftwindow", &self.window),
            ("window-memory", "repack.cruftwindowmemory", &self.window_memory),
            ("depth", "repack.cruftdepth", &self.depth),
            ("threads", "repack.cruftthreads", &self.threads),
        ] {
            if let Some(v) = pick(key, given) {
                args.push(format!("--{opt}={v}"));
            }
        }
        if self.quiet {
            args.push("-q".into());
        }
        args.push(base.display().to_string());
        args.push("--cruft".into());
        if let Some(e) = expiration {
            args.push(format!("--cruft-expiration={e}"));
        }
        args.push("--non-empty".into());
        let mut input = String::new();
        for p in packs {
            if replaced.contains(&p.file_name.as_str()) {
                input.push('-');
            }
            input.push_str(&p.file_name);
            input.push('\n');
        }
        crate::housekeeping::run_pack_objects(self.ctx, self.setup, &args, &input)
    }
}

/// `--geometric=<factor>`.
struct Geometric {
    factor: u64,
    local: bool,
    pack_kept: bool,
    delete: bool,
    quiet: bool,
}

impl Geometric {
    fn run(
        &self,
        odb: &rgit_odb::ObjectDatabase,
        pack_dir: &std::path::Path,
        objects_dir: &std::path::Path,
        keep_names: &[String],
        opts: &store::PackOptions,
    ) -> Result<(), Die> {
        let kind = odb.hash_kind();
        let local_packs = store::local_packs(odb, keep_names);
        // The packs taking part, with whether they are local.
        let mut candidates: Vec<(std::sync::Arc<rgit_pack::Pack>, bool)> = local_packs
            .iter()
            .filter(|p| (self.pack_kept || !p.keep) && !p.pack.is_cruft())
            .map(|p| (p.pack.clone(), true))
            .collect();
        if !self.local {
            let local_names: HashSet<String> =
                local_packs.iter().map(|p| p.file_name.clone()).collect();
            for alt in rgit_odb::resolve_alternates(objects_dir) {
                let set = rgit_pack::PackSet::open(alt.join("pack"), kind);
                for p in set.packs() {
                    let name = p
                        .path()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // The same pack in both places counts once.
                    if p.is_keep() || p.is_cruft() || local_names.contains(&name) {
                        continue;
                    }
                    candidates.push((p, false));
                }
            }
        }
        candidates.sort_by_key(|(p, _)| p.index().len());
        let file_name = |p: &rgit_pack::Pack| {
            p.path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let kept_names: HashSet<String> = local_packs
            .iter()
            .filter(|p| p.keep)
            .map(|p| p.file_name.clone())
            .collect();

        // Promisor packs form their own progression (and their roll-up is
        // a promisor pack again); loose objects join the other one.
        let mut groups = Vec::new();
        for promisor in [false, true] {
            let packs: Vec<(std::sync::Arc<rgit_pack::Pack>, bool)> = candidates
                .iter()
                .filter(|(p, _)| p.is_promisor() == promisor)
                .cloned()
                .collect();
            let counts: Vec<u64> = packs.iter().map(|(p, _)| u64::from(p.index().len())).collect();
            let split = geometric_split(&counts, self.factor);
            groups.push((promisor, packs, split));
        }
        let staying: Vec<&std::sync::Arc<rgit_pack::Pack>> = groups
            .iter()
            .flat_map(|(_, packs, split)| packs[*split..].iter().map(|(p, _)| p))
            .collect();
        // Objects of the packs that stay, and of kept packs, are left out.
        let excluded = |id: &ObjectId| {
            staying.iter().any(|p| p.contains(id))
                || (!self.pack_kept && local_packs.iter().any(|p| p.keep && p.pack.contains(id)))
        };

        let mut wrote = false;
        let mut removed = false;
        for (promisor, packs, split) in &groups {
            let rolled = &packs[..*split];
            let mut ids: Vec<ObjectId> = Vec::new();
            let mut seen: HashSet<ObjectId> = HashSet::new();
            for (p, _) in rolled.iter().rev() {
                let index = p.index();
                let mut by_offset: Vec<(u64, u32)> = (0..index.len())
                    .filter_map(|pos| index.offset_at(pos).ok().map(|off| (off, pos)))
                    .collect();
                by_offset.sort_unstable();
                for (_, pos) in by_offset {
                    let id = index.id_at(pos);
                    if !excluded(&id) && seen.insert(id) {
                        ids.push(id);
                    }
                }
            }
            if !promisor {
                // Loose objects go in as well; a copy in a cruft pack does
                // not count as packed.
                let mut loose = store::loose_objects(objects_dir, kind);
                loose.sort_by_key(|a| a.0);
                for (id, _, _) in loose {
                    let packed = local_packs.iter().any(|p| {
                        !p.pack.is_cruft()
                            && p.pack.contains(&id)
                            && !rolled.iter().any(|(r, _)| r.contains(&id))
                    });
                    if !packed && !excluded(&id) && seen.insert(id) {
                        ids.push(id);
                    }
                }
            }
            let described = rgit_pack::write::describe(odb, kind, &ids)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            let ordered = rgit_pack::write::pack_order(&described);
            let Some(new_pack) = store::write_pack(odb, pack_dir, &ordered, opts)? else {
                continue;
            };
            wrote = true;
            let new_name = format!("pack-{}.pack", new_pack.to_hex());
            if *promisor {
                let marker = pack_dir.join(&new_name).with_extension("promisor");
                if !marker.exists() {
                    let _ = std::fs::write(&marker, b"");
                }
            }
            if self.delete {
                for (p, is_local) in rolled {
                    let name = file_name(p);
                    if !is_local || kept_names.contains(&name) || name == new_name {
                        continue;
                    }
                    store::delete_pack(&p.path().with_extension(""));
                    removed = true;
                }
            }
        }
        if removed {
            store::remove_multi_pack_index(pack_dir);
        }
        if !wrote && !self.quiet {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", msg::REPACK_NOTHING_NEW);
        }
        Ok(())
    }
}

/// How many of the packs (object counts in ascending order) to roll up so
/// that each remaining pack holds at least `factor` times the objects of
/// the next smaller one, and of all rolled-up ones together: the largest
/// packs that already form such a progression stay, and smaller ones are
/// added to the roll-up while it outgrows the next pack.
fn geometric_split(counts: &[u64], factor: u64) -> usize {
    let n = counts.len();
    let mut split = 0;
    for i in (1..n).rev() {
        if counts[i] < factor.saturating_mul(counts[i - 1]) {
            split = i;
            break;
        }
    }
    if split == 0 {
        return 0;
    }
    let mut sum: u64 = counts[..split].iter().sum();
    while split < n && counts[split] < factor.saturating_mul(sum) {
        sum += counts[split];
        split += 1;
    }
    split
}

/// With `--unpack-unreachable=<date>`: the unreachable objects that are
/// reachable from unreachable but recent ones (loose objects, or objects
/// of packs, newer than the date). They are loosened whatever the age of
/// their pack, so that a recent object being written by another process
/// (a new tree naming an old blob) stays complete; `prune` keeps them for
/// the same reason.
fn rescue_recent(
    odb: &rgit_odb::ObjectDatabase,
    walk: &Walk<'_>,
    packs: &[store::LocalPack],
    loose: &[(ObjectId, std::path::PathBuf, u64)],
    expire: Option<u64>,
    hooked: &[ObjectId],
    common: &std::path::Path,
) -> Result<HashSet<ObjectId>, Die> {
    let Some(expire) = expire else {
        return Ok(HashSet::new());
    };
    let mut recent: Vec<ObjectId> = hooked.to_vec();
    recent.extend(loose
        .iter()
        .filter(|(id, _, mtime)| *mtime > expire && !walk.seen.contains(id))
        .map(|(id, _, _)| *id));
    recent.extend(store::recent_packed(packs, expire).filter(|id| !walk.seen.contains(id)));
    if recent.is_empty() {
        return Ok(HashSet::new());
    }
    let kind = odb.hash_kind();
    let mut extra = Walk::new(odb, reach::shallow_commits(common, kind));
    extra.seen = walk.seen.clone();
    extra.walk(&recent, true)?;
    Ok(extra
        .seen
        .into_iter()
        .filter(|id| !walk.seen.contains(id))
        .collect())
}

/// The object is available from an alternate object database.
fn in_alternate(odb: &rgit_odb::ObjectDatabase, id: &ObjectId) -> bool {
    // The first two stores are the repository's own loose objects and
    // packs; the rest belong to alternates.
    odb.stores().iter().skip(2).any(|s| s.contains(id))
}

/// Remove `.tmp-*` files left in `objects/pack` by interrupted runs of
/// other processes.
fn remove_stale_tmp_packs(pack_dir: &std::path::Path) {
    let own = format!(".tmp-{}-", std::process::id());
    let Ok(rd) = std::fs::read_dir(pack_dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".tmp-") && !name.starts_with(&own) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `-b` in a partial clone: a bitmap needs every object the pack refers
/// to in the pack, which fails when promisor objects are left out.
fn check_closure(odb: &rgit_odb::ObjectDatabase, chosen: &[ListedObject]) -> Result<(), Die> {
    let ids: HashSet<ObjectId> = chosen.iter().map(|o| o.id).collect();
    for o in chosen {
        let Ok(Some(raw)) = odb.read(&o.id) else {
            continue;
        };
        let mut refs = HashSet::new();
        rgit_odb::referenced_ids(odb.hash_kind(), raw.kind, &raw.data, &mut refs);
        let mut refs: Vec<ObjectId> = refs.into_iter().filter(|r| !ids.contains(r)).collect();
        refs.sort();
        if let Some(missing) = refs.first() {
            eprintln!("warning: {}", msg::bitmap_no_closure(&missing.to_hex()));
            return Err(Die::Fatal(msg::FAILED_TO_WRITE_BITMAP.into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::geometric_split;

    #[test]
    fn split() {
        // Already a progression.
        assert_eq!(geometric_split(&[3, 6, 12], 2), 0);
        assert_eq!(geometric_split(&[], 2), 0);
        assert_eq!(geometric_split(&[5], 2), 0);
        // The two small packs are rolled up.
        assert_eq!(geometric_split(&[3, 3, 12, 24], 2), 2);
        // The roll-up (3 + 3) grows past the 7-object pack.
        assert_eq!(geometric_split(&[3, 3, 7, 27], 2), 3);
        assert_eq!(geometric_split(&[3, 3, 6], 2), 3);
        assert_eq!(geometric_split(&[1, 3, 3, 20, 40], 2), 3);
    }
}
