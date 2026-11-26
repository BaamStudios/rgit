// SPDX-License-Identifier: MIT

//! Writing and opening reachability bitmaps for commands
//! (`Documentation/technical/bitmap-format.adoc`): `pack-objects
//! --write-bitmap-index`, `repack -b`, `multi-pack-index write --bitmap`
//! and the readers in `rev-list --use-bitmap-index`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_odb::ObjectDatabase;
use rgit_pack::Pack;
use rgit_pack::bitmap::write::{self as bw, BuildError, Options};
use rgit_pack::bitmap::{BitmapError, BitmapIndex};

use crate::messages::bitmap as msg;
use crate::prelude::*;

/// `pack.writeBitmapHashCache` and `pack.writeBitmapLookupTable`.
pub fn options(config: &ConfigSet) -> Options {
    Options {
        hash_cache: config
            .bool("pack.writebitmaphashcache")
            .ok()
            .flatten()
            .unwrap_or(true),
        lookup_table: config
            .bool("pack.writebitmaplookuptable")
            .ok()
            .flatten()
            .unwrap_or(false),
    }
}

/// The ref hierarchies of `pack.preferBitmapTips` (each ending in `/`).
/// A value-less entry is reported like git does and skipped.
pub fn prefer_patterns(config: &ConfigSet) -> Vec<String> {
    let mut out = Vec::new();
    for e in config.get_all("pack.preferbitmaptips") {
        match &e.value {
            None => eprintln!("error: {}", msg::missing_value("pack.preferbitmaptips")),
            Some(v) => {
                let mut p = v.to_string();
                if !p.ends_with('/') {
                    p.push('/');
                }
                out.push(p);
            }
        }
    }
    out
}

/// The commits at the tips of refs matching `pack.preferBitmapTips`.
pub fn preferred_tips(setup: &rgit_repo::Setup, config: &ConfigSet) -> HashSet<ObjectId> {
    let patterns = prefer_patterns(config);
    let mut out = HashSet::new();
    if patterns.is_empty() {
        return out;
    }
    let refs = crate::refs::Refs::new(setup);
    for (name, oid) in refs.list("refs/") {
        if patterns.iter().any(|p| name.starts_with(p.as_str())) {
            out.insert(oid);
        }
    }
    out
}

/// Write the `.bitmap` of the pack whose index is `idx` (just written by
/// `pack-objects --all --write-bitmap-index`). `walk_commits` are the
/// commits in the order the walk found them.
pub fn write_for_pack(
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    odb: &ObjectDatabase,
    idx: &Path,
    walk_commits: &[ObjectId],
    progress: bool,
) -> Result<(), Die> {
    let hash = setup.format.object_format;
    let pack = Pack::open(idx, hash).map_err(|e| Die::Fatal(e.to_string()))?;
    let preferred = preferred_tips(setup, config);
    let opts = options(config);
    let written = match bw::write_pack_bitmap(
        &pack,
        odb,
        Some(walk_commits),
        &|id| preferred.contains(id),
        opts,
    ) {
        Ok(w) => w,
        Err(BuildError::Missing(id)) => {
            eprintln!("warning: {}", msg::no_full_closure(&id.to_hex()));
            return Err(Die::Fatal(msg::FAILED_TO_WRITE.into()));
        }
        Err(e) => return Err(Die::Fatal(e.to_string())),
    };
    if progress {
        let n = written.selected as u64;
        let mut meter = rgit_progress::Progress::start(msg::PROGRESS_BUILDING, n, true);
        meter.set(n);
        meter.stop();
    }
    if opts.lookup_table {
        let _r = rgit_trace::trace2::region("pack-bitmap-write", "writing_lookup_table", true);
    }
    rgit_trace::trace2::data_int(
        "pack-bitmap-write",
        "num_selected_commits",
        written.selected as i64,
        true,
    );
    rgit_trace::trace2::data_int(
        "pack-bitmap-write",
        "num_maximal_commits",
        written.maximal as i64,
        true,
    );
    write_file(&idx.with_extension("bitmap"), &written.bytes)
}

/// Write `bytes` to `path` through a temporary file, read-only.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), Die> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!("tmp_bitmap_{}", std::process::id()));
    let fail = |e: std::io::Error| Die::Fatal(format!("unable to write '{}': {e}", path.display()));
    std::fs::write(&tmp, bytes).map_err(fail)?;
    set_read_only(&tmp);
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        fail(e)
    })
}

#[cfg(unix)]
fn set_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
}

#[cfg(not(unix))]
fn set_read_only(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut p = meta.permissions();
        p.set_readonly(true);
        let _ = std::fs::set_permissions(path, p);
    }
}

/// Print the errors of a bitmap that could not be loaded.
pub fn report(e: &BitmapError) {
    for m in &e.0 {
        eprintln!("error: {m}");
    }
}

/// Open the bitmap of the repository: the MIDX's if it has one, else the
/// first pack (newest first) with a `.bitmap`. Problems are printed;
/// `None` means there is no usable bitmap.
pub fn open(set: &rgit_pack::PackSet) -> Option<BitmapIndex> {
    let packs: Vec<Arc<Pack>> = set.packs();
    if let Some(chain) = set.midx()
        && !chain.is_incremental()
    {
        let mut errors = Vec::new();
        if let Ok(Some(midx)) =
            rgit_pack::midx::MultiPackIndex::open(chain.tip().path(), set.hash_kind(), &mut errors)
        {
            let read_ridx = std::env::var("GIT_TEST_MIDX_READ_RIDX").map_or(true, |v| v != "0");
            match BitmapIndex::open_midx(set.dir(), midx, &packs, read_ridx) {
                Ok(Some(b)) => {
                    if let Some(src) = b.revindex_source() {
                        rgit_trace::trace2::data("load_midx_revindex", "source", src, true);
                    }
                    rgit_trace::trace2::data(
                        "bitmap",
                        "opened bitmap file",
                        &b.path().display().to_string(),
                        true,
                    );
                    return Some(b);
                }
                Ok(None) => {}
                Err(e) => {
                    report(&e);
                    return None;
                }
            }
        }
    }
    let mut found: Option<BitmapIndex> = None;
    // git looks at the packs in the order it loaded them (oldest name
    // first); the first one with a bitmap wins.
    let mut ordered = packs.clone();
    ordered.sort_by(|a, b| a.path().cmp(b.path()));
    for p in ordered {
        if !rgit_pack::bitmap::pack_bitmap_path(p.index().path()).exists() {
            continue;
        }
        if let Some(f) = &found {
            rgit_trace::trace2::data(
                "bitmap",
                "ignoring extra bitmap file",
                &p.path().display().to_string(),
                true,
            );
            let _ = f;
            continue;
        }
        match BitmapIndex::open_pack(p.clone()) {
            Ok(Some(b)) => {
                rgit_trace::trace2::data(
                    "bitmap",
                    "opened bitmap file",
                    &p.path().display().to_string(),
                    true,
                );
                found = Some(b);
            }
            Ok(None) => {}
            Err(e) => {
                report(&e);
                return None;
            }
        }
    }
    found
}

/// The name hashes stored in the bitmaps of `pack_dir` (pack bitmaps and
/// the MIDX bitmap), by object. A MIDX bitmap reuses them, as git does.
pub fn existing_name_hashes(
    pack_dir: &Path,
    hash: rgit_hash::HashKind,
) -> std::collections::HashMap<ObjectId, u32> {
    let mut out = std::collections::HashMap::new();
    let set = rgit_pack::PackSet::open(pack_dir, hash);
    let packs = set.packs();
    let mut add = |b: &BitmapIndex| {
        if !b.has_hash_cache() {
            return;
        }
        for bit in 0..b.num_objects() {
            if let Some(h) = b.name_hash(bit)
                && h != 0
            {
                out.entry(b.id_at(bit)).or_insert(h);
            }
        }
    };
    for p in &packs {
        if let Ok(Some(b)) = BitmapIndex::open_pack(p.clone()) {
            add(&b);
        }
    }
    let mut errors = Vec::new();
    if let Ok(Some(m)) = rgit_pack::midx::MultiPackIndex::open(
        pack_dir.join(rgit_pack::midx::FILE_NAME),
        hash,
        &mut errors,
    ) && let Ok(Some(b)) = BitmapIndex::open_midx(pack_dir, m, &packs, true)
    {
        add(&b);
    }
    out
}

/// The tips of `--refs-snapshot=<path>`: object ids, a leading `+` marks
/// a preferred tip.
pub fn read_refs_snapshot(path: &Path, hash: rgit_hash::HashKind) -> Result<Vec<(ObjectId, bool)>, Die> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Die::Fatal(format!("could not open '{}': {e}", path.display())))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let (pref, hex) = match line.strip_prefix('+') {
            Some(rest) => (true, rest),
            None => (false, line),
        };
        let id = ObjectId::from_hex(hash, hex.trim().as_bytes())
            .map_err(|_| Die::Fatal(format!("could not parse line: {line}")))?;
        out.push((id, pref));
    }
    Ok(out)
}

/// Write the `.bitmap` of the MIDX just written to `pack_dir` (`write
/// --bitmap`). The candidates are the commits reachable from the refs (or
/// the snapshot), in walk order; name hashes come from `hashes`.
pub fn write_for_midx(
    ctx: &Context,
    setup: &rgit_repo::Setup,
    config: &ConfigSet,
    pack_dir: &Path,
    hashes: &std::collections::HashMap<ObjectId, u32>,
    snapshot: Option<&[(ObjectId, bool)]>,
) -> Result<(), Die> {
    let hash = setup.format.object_format;
    let mut errors = Vec::new();
    let Some(midx) = rgit_pack::midx::MultiPackIndex::open(
        pack_dir.join(rgit_pack::midx::FILE_NAME),
        hash,
        &mut errors,
    )
    .map_err(|e| Die::Fatal(e.0))?
    else {
        return Ok(());
    };
    let Some(order) = midx.reverse_index() else {
        return Ok(());
    };
    let set = rgit_pack::PackSet::open(pack_dir, hash);
    let all_packs = set.packs();
    let packs: Vec<Option<Arc<Pack>>> = midx
        .pack_names()
        .iter()
        .map(|n| {
            let idx = pack_dir.join(n);
            all_packs.iter().find(|p| p.index().path() == idx).cloned()
        })
        .collect();
    let mut objects = Vec::with_capacity(order.len());
    let mut commits_in: HashSet<ObjectId> = HashSet::new();
    for &pos in &order {
        let id = midx.id_at(pos);
        let pid = midx.pack_id(pos).map_err(Die::Fatal)?;
        let off = midx.offset(pos).map_err(Die::Fatal)?;
        let pack = packs
            .get(pid as usize)
            .cloned()
            .flatten()
            .ok_or_else(|| Die::Fatal(format!("could not load pack {pid}")))?;
        let (kind, _) = pack.object_info(off).map_err(|e| Die::Fatal(e.to_string()))?;
        if kind == rgit_object::ObjectKind::Commit {
            commits_in.insert(id);
        }
        objects.push((id, kind));
    }
    let mut preferred = preferred_tips(setup, config);
    let args: Vec<String> = match snapshot {
        Some(tips) => {
            for (id, p) in tips {
                if *p {
                    preferred.insert(*id);
                }
            }
            let mut v: Vec<String> = tips.iter().map(|(id, _)| id.to_hex()).collect();
            v.sort();
            v.dedup();
            v
        }
        None => vec!["--all".to_owned()],
    };
    let walked = if args.is_empty() {
        Vec::new()
    } else {
        crate::history::walk_commits(ctx, &args)?
    };
    let candidates: Vec<ObjectId> = walked.into_iter().filter(|c| commits_in.contains(c)).collect();
    let names: Vec<u32> = objects
        .iter()
        .map(|(id, _)| hashes.get(id).copied().unwrap_or(0))
        .collect();
    let midx_sum = midx.checksum();
    let objs = bw::Objects {
        objects: &objects,
        index_pos: &order,
        checksum: midx_sum.as_bytes(),
    };
    let store = crate::repo_objects::Repo::open(ctx, setup.clone())?;
    let opts = options(config);
    let written = match bw::write_bitmap(
        &store.odb,
        hash,
        &objs,
        &candidates,
        &|id| preferred.contains(id),
        bw::NameHashes::Given(names),
        opts,
    ) {
        Ok(w) => w,
        Err(BuildError::Missing(id)) => {
            return Err(Die::Fatal(msg::midx_missing(&id.to_hex())));
        }
        Err(e) => return Err(Die::Fatal(e.to_string())),
    };
    if opts.lookup_table {
        let _r = rgit_trace::trace2::region("pack-bitmap-write", "writing_lookup_table", true);
    }
    write_file(
        &rgit_pack::bitmap::midx_bitmap_path(pack_dir, &midx.checksum()),
        &written.bytes,
    )
}

/// How many of `objects` git would send verbatim from the bitmapped pack
/// (the preferred pack of a MIDX bitmap): those stored there.
pub fn reusable_count(odb: &ObjectDatabase, objects: &[rgit_pack::write::ListedObject]) -> usize {
    let Some(store) = odb.packs() else {
        return 0;
    };
    let Some(index) = open(store.set()) else {
        return 0;
    };
    let first = (index.num_objects() > 0)
        .then(|| index.location(0))
        .flatten()
        .map(|(p, _)| p.uid());
    objects
        .iter()
        .filter(|o| match index.position(&o.id) {
            None => false,
            Some(bit) => {
                !index.is_midx()
                    || index.location(bit).map(|(p, _)| p.uid()) == first
            }
        })
        .count()
}
