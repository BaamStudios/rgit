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
