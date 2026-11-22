// SPDX-License-Identifier: MIT
//! `git repack` (Documentation/git-repack.adoc,
//! Documentation/config/repack.adoc).
//!
//! Without `-a`/`-A` the reachable loose objects are put into a new pack
//! (`-d` then removes their loose copies). With `-a` everything reachable
//! goes into one new pack; with `-d` the old packs are deleted afterwards,
//! and with `-A` the unreachable objects of those packs are first written
//! as loose objects carrying the pack's modification time (unless older
//! than `--unpack-unreachable`). `-k` keeps unreachable objects in the new
//! pack instead. Packs with a `.keep` file or named by `--keep-pack` are
//! neither repacked nor deleted. The new pack is written by
//! `rgit_pack::write` (packed data is reused; delta search comes with
//! #19); `--window`, `--window-memory` and `--threads` are accepted for
//! it. `update-server-info` runs unless `-n` or `repack.updateServerInfo`
//! is false.
//!
//! Not yet: `--cruft` (#96), `--geometric`, `--write-midx` (#21),
//! `--write-bitmap-index` (#23), `--filter` (#24), `--max-pack-size`.

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
    let delete = Cell::new(false);
    let no_reuse_delta = Cell::new(false);
    let no_reuse_object = Cell::new(false);
    let no_server_info = Cell::new(false);
    let quiet = Cell::new(false);
    let local = Cell::new(false);
    let bitmap = Cell::new(false);
    let islands = Cell::new(false);
    let unpack_unreachable: RefCell<Option<String>> = RefCell::new(None);
    let keep_unreachable = Cell::new(false);
    let window: RefCell<Option<String>> = RefCell::new(None);
    let window_memory: RefCell<Option<String>> = RefCell::new(None);
    let depth: RefCell<Option<String>> = RefCell::new(None);
    let threads: RefCell<Option<String>> = RefCell::new(None);
    let pack_kept = Cell::new(false);
    let keep_packs: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let path_walk = Cell::new(false);
    let name_hash_version: RefCell<Option<String>> = RefCell::new(None);
    // Options that change the result and are not implemented yet: the
    // first one given is refused.
    let unsupported: RefCell<Option<String>> = RefCell::new(None);
    let reject = |name: &'static str| {
        let unsupported = &unsupported;
        move |_: Option<&str>, unset: bool| -> Result<(), CallbackError> {
            if !unset {
                unsupported.borrow_mut().get_or_insert_with(|| format!("--{name}"));
            }
            Ok(())
        }
    };
    let mut parser = Parser::new(
        msg::REPACK_USAGE,
        vec![
            Opt::flag('a', None, &all, msg::REPACK_H_A),
            Opt::flag('A', None, &all_loosen, msg::REPACK_H_BIG_A),
            Opt::flag(None, "cruft", &cruft, msg::REPACK_H_CRUFT),
            Opt::callback(
                None,
                "cruft-expiration",
                Some("approxidate"),
                msg::REPACK_H_CRUFT_EXPIRATION,
                reject("cruft-expiration"),
            ),
            Opt::callback(
                None,
                "combine-cruft-below-size",
                Some("n"),
                msg::REPACK_H_COMBINE_CRUFT,
                reject("combine-cruft-below-size"),
            )
            .noneg(),
            Opt::callback(
                None,
                "max-cruft-size",
                Some("n"),
                msg::REPACK_H_MAX_CRUFT,
                reject("max-cruft-size"),
            )
            .noneg(),
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
            Opt::flag('b', "write-bitmap-index", &bitmap, msg::REPACK_H_BITMAP),
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
            Opt::callback(
                None,
                "max-pack-size",
                Some("n"),
                msg::REPACK_H_MAX_PACK_SIZE,
                reject("max-pack-size"),
            )
            .noneg(),
            Opt::callback(None, "filter", Some("args"), msg::REPACK_H_FILTER, reject("filter")),
            Opt::flag(None, "pack-kept-objects", &pack_kept, msg::REPACK_H_PACK_KEPT),
            Opt::string_list(None, "keep-pack", &keep_packs, "name", msg::REPACK_H_KEEP_PACK),
            Opt::callback(
                'g',
                "geometric",
                Some("n"),
                msg::REPACK_H_GEOMETRIC,
                reject("geometric"),
            ),
            Opt::callback(
                'm',
                "write-midx",
                Some("mode"),
                msg::REPACK_H_WRITE_MIDX,
                reject("write-midx"),
            )
            .optional(None),
            Opt::callback(None, "expire-to", Some("dir"), msg::REPACK_H_EXPIRE_TO, reject("expire-to")),
            Opt::callback(None, "filter-to", Some("dir"), msg::REPACK_H_FILTER_TO, reject("filter-to")),
            Opt::callback(
                None,
                "drop-filtered",
                None,
                msg::REPACK_H_DROP_FILTERED,
                reject("drop-filtered"),
            )
            .noarg(),
            Opt::callback(None, "dry-run", None, msg::REPACK_H_DRY_RUN, reject("dry-run")).noarg(),
        ],
    );
    let _args = parser.parse(argv)?.args;
    drop(parser);
    if keep_unreachable.get() && cruft.get() {
        return Err(Die::Fatal(msg::REPACK_KEEP_AND_CRUFT.into()));
    }
    if cruft.get() {
        unsupported.borrow_mut().get_or_insert_with(|| "--cruft".into());
    }
    if bitmap.get() {
        unsupported
            .borrow_mut()
            .get_or_insert_with(|| "--write-bitmap-index".into());
    }
    if let Some(opt) = unsupported.borrow().as_deref() {
        return Err(Die::Fatal(msg::not_supported(opt)));
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = repo::load_config(ctx, &setup)?;
    let all_loosen = all_loosen.get();
    let pack_everything = all.get() || all_loosen;
    let delete = delete.get();
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
    if let Some(d) = depth.take() {
        let d: i64 = d.trim().parse().unwrap_or(50);
        opts.write.max_depth = d.clamp(0, i64::from(rgit_pack::write::MAX_DEPTH)) as u32;
    }
    let pack_kept = pack_kept.get()
        || config
            .bool("repack.packkeptobjects")
            .ok()
            .flatten()
            .unwrap_or(false);
    let keep_names = keep_packs.take();

    let odb = repo::odb_with_config(&setup, &config)?;
    let kind = setup.format.object_format;
    let objects_dir = rgit_odb::objects_dir(&setup.common_dir);
    let pack_dir = objects_dir.join("pack");
    let common = setup.absolute_common_dir();
    let packs = store::local_packs(&odb, &keep_names);
    let kept: Vec<&store::LocalPack> = packs.iter().filter(|p| p.keep).collect();
    let in_kept = |id: &ObjectId| kept.iter().any(|p| p.pack.contains(id));
    let in_local_pack = |id: &ObjectId| packs.iter().any(|p| p.pack.contains(id));
    let loose: Vec<(ObjectId, std::path::PathBuf, u64)> =
        store::loose_objects(&objects_dir, kind);
    let loose_ids: HashSet<ObjectId> = loose.iter().map(|(id, _, _)| *id).collect();
    let is_local = |id: &ObjectId| loose_ids.contains(id) || in_local_pack(id);

    // What is reachable.
    let mut walk = Walk::new(&odb, reach::shallow_commits(&common, kind));
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
    let ordered = rgit_pack::write::pack_order(&chosen);
    let new_pack = store::write_pack(&odb, &pack_dir, &ordered, &opts)?;
    if new_pack.is_none() && !quiet.get() {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", msg::REPACK_NOTHING_NEW);
    }
    let new_name = new_pack.map(|c| format!("pack-{}.pack", c.to_hex()));
    let new_ids: HashSet<ObjectId> = ordered.iter().map(|o| o.id).collect();

    if delete {
        if pack_everything {
            let old: Vec<&store::LocalPack> = packs
                .iter()
                .filter(|p| !p.keep && Some(&p.file_name) != new_name.as_ref())
                .collect();
            if all_loosen && !keep_unreachable.get() {
                for p in &old {
                    if unpack_expire.is_some_and(|t| p.mtime < t) {
                        continue;
                    }
                    for id in p.pack.index().ids() {
                        if walk.seen.contains(&id)
                            || new_ids.contains(&id)
                            || in_kept(&id)
                            || loose_ids.contains(&id)
                            || in_alternate(&odb, &id)
                        {
                            continue;
                        }
                        store::loosen(&odb, &id, p.mtime)?;
                    }
                }
            }
            if !old.is_empty() {
                store::remove_multi_pack_index(&pack_dir);
            }
            for p in old {
                store::delete_pack(&p.base());
            }
            remove_stale_tmp_packs(&pack_dir);
        }
        let packs_now = store::local_packs(&odb, &keep_names);
        store::prune_packed(&objects_dir, &objects_dir, kind, &packs_now, false)?;
    }

    let update_info = !no_server_info.get()
        && config
            .bool("repack.updateserverinfo")
            .ok()
            .flatten()
            .unwrap_or(true);
    if update_info {
        let packs_now = store::local_packs(&odb, &keep_names);
        let _ = server_info::update(&setup, &odb, &packs_now);
    }
    Ok(0)
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
