// SPDX-License-Identifier: MIT

//! Writing multi-pack indexes (`git multi-pack-index write`).
//!
//! The layout follows `Documentation/gitformat-pack.adoc`: a 12-byte header,
//! the chunk table, the chunks `PNAM`, `OIDF`, `OIDL`, `OOFF` and (only when
//! an offset needs more than 32 bits) `LOFF`, then the checksum of all of
//! it. Behaviour determined from the git binary (2.56):
//!
//! - packs are listed by `.idx` file name in byte order; the pack-int-id is
//!   the position in that list;
//! - an object stored in several packs is taken from the preferred pack,
//!   else from the pack whose `.pack` file has the newest modification time
//!   (with nanoseconds), else from the pack with the lowest pack-int-id;
//! - with large offsets, offsets of 2^31 and above go to `LOFF`, in object
//!   order.

use std::collections::{BTreeSet, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rgit_fs::{LockFile, LockOptions};
use rgit_hash::{HashKind, ObjectId};

use super::messages as m;
use super::{
    CHAIN_DIR, CHAIN_FILE, FILE_NAME, HEADER_LEN, ID_LOFF, ID_OIDF, ID_OIDL, ID_OOFF, ID_PNAM,
    LARGE_FLAG, MidxChain, MultiPackIndex, SIGNATURE, hash_version, layer_path,
};
use crate::PackIndex;

/// One pack to index.
pub struct PackInput<'a> {
    /// The `.idx` file name (`pack-<hash>.idx`).
    pub name: String,
    /// Its index.
    pub index: &'a PackIndex,
    /// Modification time of the `.pack` file in nanoseconds since the
    /// epoch.
    pub mtime: i128,
}

/// A selected object: pack-int-id and offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The object.
    pub id: ObjectId,
    /// Pack-int-id.
    pub pack: u32,
    /// Offset in that pack.
    pub offset: u64,
}

/// Select one copy of every object of `packs` (in pack-int-id order) that
/// `skip` does not exclude: from `preferred`, else the newest pack, else the
/// lowest pack-int-id. Sorted by id.
pub fn select_entries(
    packs: &[PackInput<'_>],
    preferred: Option<usize>,
    skip: &dyn Fn(&ObjectId) -> bool,
) -> Result<Vec<Entry>, crate::Error> {
    let mut all: Vec<(Entry, bool, i128)> = Vec::new();
    for (i, p) in packs.iter().enumerate() {
        let pref = preferred == Some(i);
        for pos in 0..p.index.len() {
            let id = p.index.id_at(pos);
            if skip(&id) {
                continue;
            }
            let offset = p.index.offset_at(pos)?;
            all.push((
                Entry {
                    id,
                    pack: i as u32,
                    offset,
                },
                pref,
                p.mtime,
            ));
        }
    }
    all.sort_by(|a, b| {
        a.0.id
            .cmp(&b.0.id)
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.0.pack.cmp(&b.0.pack))
    });
    let mut out: Vec<Entry> = Vec::with_capacity(all.len());
    for (e, _, _) in all {
        if out.last().is_some_and(|l| l.id == e.id) {
            continue;
        }
        out.push(e);
    }
    Ok(out)
}

/// The bytes of a MIDX listing `names` (pack-int-id order) and `entries`
/// (sorted by id), including the trailing checksum.
pub fn build(kind: HashKind, version: u8, names: &[&str], entries: &[Entry]) -> Vec<u8> {
    let h = kind.byte_len();
    let mut pnam: Vec<u8> = Vec::new();
    for n in names {
        pnam.extend_from_slice(n.as_bytes());
        pnam.push(0);
    }
    while pnam.len() % 4 != 0 {
        pnam.push(0);
    }
    let mut fanout = vec![0u32; 256];
    for e in entries {
        fanout[usize::from(e.id.as_bytes()[0])] += 1;
    }
    let mut acc = 0u32;
    let mut oidf = Vec::with_capacity(1024);
    for f in &fanout {
        acc += f;
        oidf.extend_from_slice(&acc.to_be_bytes());
    }
    let mut oidl = Vec::with_capacity(entries.len() * h);
    for e in entries {
        oidl.extend_from_slice(e.id.as_bytes());
    }
    let need_large = entries.iter().any(|e| e.offset > u64::from(u32::MAX));
    let mut ooff = Vec::with_capacity(entries.len() * 8);
    let mut loff = Vec::new();
    for e in entries {
        ooff.extend_from_slice(&e.pack.to_be_bytes());
        if need_large && e.offset > 0x7fff_ffff {
            let i = (loff.len() / 8) as u32;
            ooff.extend_from_slice(&(LARGE_FLAG | i).to_be_bytes());
            loff.extend_from_slice(&e.offset.to_be_bytes());
        } else {
            ooff.extend_from_slice(&(e.offset as u32).to_be_bytes());
        }
    }
    let mut chunks: Vec<(u32, Vec<u8>)> = vec![
        (ID_PNAM, pnam),
        (ID_OIDF, oidf),
        (ID_OIDL, oidl),
        (ID_OOFF, ooff),
    ];
    if need_large {
        chunks.push((ID_LOFF, loff));
    }
    let mut out = Vec::new();
    out.extend_from_slice(&SIGNATURE.to_be_bytes());
    out.push(version);
    out.push(hash_version(kind));
    out.push(chunks.len() as u8);
    out.push(0);
    out.extend_from_slice(&(names.len() as u32).to_be_bytes());
    let mut offset = (HEADER_LEN + (chunks.len() + 1) * 12) as u64;
    for (id, data) in &chunks {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&offset.to_be_bytes());
        offset += data.len() as u64;
    }
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&offset.to_be_bytes());
    for (_, data) in &chunks {
        out.extend_from_slice(data);
    }
    let sum = rgit_hash::hash(kind, &out).expect("no collision attack in own output");
    out.extend_from_slice(sum.as_bytes());
    out
}

/// Errors of [`write`]. `Display` is git's message.
#[derive(Debug)]
pub enum WriteError {
    /// git prints `error: <msg>` and exits with 255.
    Error(String),
    /// git dies: `fatal: <msg>`.
    Fatal(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Error(s) | WriteError::Fatal(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for WriteError {}

/// Options of [`write`].
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// `midx.version` (1 or 2; 0 means 1).
    pub version: u8,
    /// `--preferred-pack`.
    pub preferred: Option<String>,
    /// `--stdin-packs`: only these `.idx` names.
    pub only: Option<BTreeSet<String>>,
    /// `--bitmap` (bitmaps are not written yet, #23; this only disables
    /// keeping an up-to-date MIDX without one).
    pub bitmap: bool,
    /// `--incremental`.
    pub incremental: bool,
    /// `--no-write-chain-file` (with `incremental`).
    pub no_chain_file: bool,
    /// `--base=<checksum>`: `Some(None)` for `none`.
    pub base: Option<Option<String>>,
    /// Show `Adding packfiles to multi-pack-index`.
    pub progress: bool,
    /// Packs to leave out although present (deleted by `expire`).
    pub drop: BTreeSet<String>,
}

/// What [`write`] did.
#[derive(Debug, Default)]
pub struct WriteOutcome {
    /// Checksum of the new file (`None` if nothing was written).
    pub written: Option<ObjectId>,
    /// Warnings (`warning: <text>`), already in output order.
    pub warnings: Vec<String>,
}

/// A pack found in the directory.
struct Found {
    name: String,
    index: PackIndex,
    mtime: i128,
}

fn mtime_nanos(path: &Path) -> Option<i128> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    })
}

/// `.idx` file names in `pack_dir`, sorted.
pub fn idx_names(pack_dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(pack_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(".idx"))
        .collect();
    out.sort();
    out
}

/// Write the MIDX of `pack_dir` (`git multi-pack-index write`).
///
/// `display_dir` is the pack directory as git names it in messages (for
/// example `.git/objects/pack`). Warnings and progress go to standard
/// error; the checksum of a layer written with `no_chain_file` is the
/// caller's to print.
pub fn write(
    pack_dir: &Path,
    display_dir: &str,
    kind: HashKind,
    opts: &WriteOptions,
) -> Result<WriteOutcome, WriteError> {
    let mut outcome = WriteOutcome::default();
    let version = if opts.version == 0 { 1 } else { opts.version };

    // The existing MIDX (an unusable one is ignored, as git does).
    let mut load_errors = Vec::new();
    let existing = MidxChain::open(pack_dir, kind, &mut load_errors);
    for e in &load_errors {
        eprintln!("error: {e}");
    }
    let existing = existing.map_err(|e| WriteError::Fatal(e.0))?;
    let existing = match existing {
        Some(c) if !c.is_incremental() && !c.tip().checksum_ok() => {
            outcome.warnings.push(m::IGNORING_CHECKSUM_MISMATCH.into());
            eprintln!("warning: {}", m::IGNORING_CHECKSUM_MISMATCH);
            None
        }
        other => other,
    };

    // Packs of the existing MIDX must still load.
    if let Some(c) = &existing
        && !opts.incremental
        && !c.is_incremental()
    {
        let tip = c.tip();
        for (i, name) in tip.pack_names().iter().enumerate() {
            if opts.drop.contains(name) {
                continue;
            }
            if let Some(only) = &opts.only
                && !only.contains(name)
            {
                continue;
            }
            let idx = pack_dir.join(name);
            if !idx.exists() {
                continue;
            }
            let ok = idx.with_extension("pack").exists() && PackIndex::open(&idx, kind).is_ok();
            if !ok {
                return Err(WriteError::Error(m::could_not_load_pack(i as u32)));
            }
        }
    }

    // Layers below a new incremental layer.
    let mut base_layers: Vec<MultiPackIndex> = Vec::new();
    let mut convert_single: Option<ObjectId> = None;
    if opts.incremental
        && let Some(c) = existing
    {
        if !c.is_incremental() {
            convert_single = Some(c.tip().checksum());
        }
        base_layers = c.into_layers();
        if let Some(base) = &opts.base {
            match base {
                None => base_layers.clear(),
                Some(hex) => {
                    let Some(pos) = base_layers
                        .iter()
                        .position(|l| l.checksum().to_hex() == *hex)
                    else {
                        return Err(WriteError::Error(format!("could not find MIDX: {hex}")));
                    };
                    base_layers.truncate(pos + 1);
                }
            }
        }
    }

    // The packs of the directory.
    let mut packs: Vec<Found> = Vec::new();
    let mut progress = rgit_progress::Progress::start(m::PROGRESS_ADDING, 0, opts.progress);
    for name in idx_names(pack_dir) {
        if opts.drop.contains(&name) {
            continue;
        }
        if let Some(only) = &opts.only
            && !only.contains(&name)
        {
            continue;
        }
        if opts.incremental && base_layers.iter().any(|l| l.contains_pack(&name)) {
            continue;
        }
        let idx = pack_dir.join(&name);
        let pack = idx.with_extension("pack");
        let (Some(mtime), Ok(index)) = (mtime_nanos(&pack), PackIndex::open(&idx, kind)) else {
            let w = m::failed_to_add_packfile(&format!("{display_dir}/{name}"));
            eprintln!("warning: {w}");
            outcome.warnings.push(w);
            continue;
        };
        packs.push(Found { name, index, mtime });
        progress.inc();
    }
    progress.stop();

    let names: Vec<&str> = packs.iter().map(|p| p.name.as_str()).collect();

    // An up-to-date stand-alone MIDX is kept.
    if !opts.incremental {
        let mut errors = Vec::new();
        if let Ok(Some(c)) = MidxChain::open(pack_dir, kind, &mut errors)
            && !c.is_incremental()
        {
            let tip = c.tip();
            let bitmap_ok = !opts.bitmap
                || pack_dir
                    .join(format!(
                        "multi-pack-index-{}.bitmap",
                        tip.checksum().to_hex()
                    ))
                    .exists();
            if tip.version() == version
                && tip.checksum_ok()
                && tip
                    .pack_names()
                    .iter()
                    .map(String::as_str)
                    .eq(names.iter().copied())
                && bitmap_ok
            {
                return Ok(outcome);
            }
        }
    }

    let mut preferred = None;
    if let Some(p) = &opts.preferred {
        let stem = p
            .strip_suffix(".idx")
            .or_else(|| p.strip_suffix(".pack"))
            .unwrap_or("\0");
        match packs
            .iter()
            .position(|f| f.name.strip_suffix(".idx") == Some(stem))
        {
            Some(i) => preferred = Some(i),
            None => {
                let w = m::unknown_preferred(p);
                eprintln!("warning: {w}");
                outcome.warnings.push(w);
            }
        }
    }
    if packs.is_empty() {
        if opts.incremental {
            return Ok(outcome);
        }
        return Err(WriteError::Error(m::NO_PACKS.into()));
    }
    if let Some(i) = preferred
        && packs[i].index.is_empty()
    {
        let name = packs[i].name.strip_suffix(".idx").unwrap_or(&packs[i].name);
        return Err(WriteError::Error(m::preferred_empty(&format!(
            "{display_dir}/{name}.pack"
        ))));
    }

    let inputs: Vec<PackInput<'_>> = packs
        .iter()
        .map(|f| PackInput {
            name: f.name.clone(),
            index: &f.index,
            mtime: f.mtime,
        })
        .collect();
    let skip = |id: &ObjectId| base_layers.iter().any(|l| l.lookup(id).is_some());
    let entries =
        select_entries(&inputs, preferred, &skip).map_err(|e| WriteError::Fatal(e.to_string()))?;
    let bytes = build(kind, version, &names, &entries);
    let sum =
        ObjectId::from_bytes(kind, &bytes[bytes.len() - kind.byte_len()..]).expect("hash length");

    if opts.incremental {
        let dir = pack_dir.join(CHAIN_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| WriteError::Fatal(e.to_string()))?;
        if let Some(old) = convert_single {
            // The stand-alone MIDX becomes the base layer.
            let _ = std::fs::rename(pack_dir.join(FILE_NAME), layer_path(pack_dir, &old));
            for ext in ["bitmap", "rev"] {
                let from = pack_dir.join(format!("multi-pack-index-{}.{ext}", old.to_hex()));
                if from.exists() {
                    let _ = std::fs::rename(
                        &from,
                        dir.join(format!("multi-pack-index-{}.{ext}", old.to_hex())),
                    );
                }
            }
        }
        write_file(&layer_path(pack_dir, &sum), &bytes)?;
        if !opts.no_chain_file {
            let mut text = String::new();
            for l in &base_layers {
                text.push_str(&l.checksum().to_hex());
                text.push('\n');
            }
            text.push_str(&sum.to_hex());
            text.push('\n');
            write_file(&dir.join(CHAIN_FILE), text.as_bytes())?;
            let keep: HashSet<String> = base_layers
                .iter()
                .map(|l| l.checksum().to_hex())
                .chain([sum.to_hex()])
                .collect();
            clean_chain_dir(&dir, Some(&keep));
        }
    } else {
        write_file(&pack_dir.join(FILE_NAME), &bytes)?;
        clean_chain_dir(&pack_dir.join(CHAIN_DIR), None);
    }
    clean_stale_files(pack_dir, &sum);
    outcome.written = Some(sum);
    Ok(outcome)
}

/// Replace `path` with `bytes` through `path.lock`.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), WriteError> {
    let mut lock = LockFile::acquire(path, &LockOptions::new())
        .map_err(|e| WriteError::Fatal(e.to_string()))?;
    lock.file_mut()
        .write_all(bytes)
        .map_err(|e| WriteError::Fatal(e.to_string()))?;
    lock.commit().map_err(|e| WriteError::Fatal(e.to_string()))
}

/// Remove `multi-pack-index-<hash>.{rev,bitmap}` of other MIDXs from
/// `pack_dir`.
fn clean_stale_files(pack_dir: &Path, keep: &ObjectId) {
    let keep = keep.to_hex();
    for e in std::fs::read_dir(pack_dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("multi-pack-index-") else {
            continue;
        };
        let stem = rest
            .strip_suffix(".rev")
            .or_else(|| rest.strip_suffix(".bitmap"));
        if let Some(stem) = stem
            && stem != keep
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Remove the layers of the chain directory (and their `.bitmap`/`.rev`)
/// except those named in `keep`; without `keep` also the chain file.
fn clean_chain_dir(dir: &Path, keep: Option<&HashSet<String>>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == CHAIN_FILE {
            if keep.is_none() {
                let _ = std::fs::remove_file(e.path());
            }
            continue;
        }
        // Only layers and their bitmaps and reverse indexes.
        let Some((hash, ext)) = name
            .strip_prefix("multi-pack-index-")
            .and_then(|r| r.split_once('.'))
        else {
            continue;
        };
        if !matches!(ext, "midx" | "bitmap" | "rev")
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || keep.is_some_and(|k| k.contains(hash))
        {
            continue;
        }
        let _ = std::fs::remove_file(e.path());
    }
}

/// Paths of the files that belong to the pack whose `.idx` is `idx`.
pub fn pack_files(idx: &Path) -> Vec<PathBuf> {
    ["pack", "idx", "rev", "bitmap", "promisor", "mtimes"]
        .iter()
        .map(|ext| idx.with_extension(ext))
        .collect()
}

/// A version 2 MIDX combining the consecutive chain layers `layers`
/// (`git multi-pack-index compact`): their packs in layer order, their
/// objects with the pack-int-ids shifted accordingly.
pub fn compact_bytes(kind: HashKind, layers: &[MultiPackIndex]) -> Result<Vec<u8>, WriteError> {
    let mut names: Vec<&str> = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    for l in layers {
        let shift = names.len() as u32;
        names.extend(l.pack_names().iter().map(String::as_str));
        for pos in 0..l.len() {
            let pack = l.pack_id(pos).map_err(WriteError::Fatal)?;
            let offset = l.offset(pos).map_err(WriteError::Fatal)?;
            entries.push(Entry {
                id: l.id_at(pos),
                pack: pack + shift,
                offset,
            });
        }
    }
    entries.sort_by_key(|a| a.id);
    entries.dedup_by(|b, a| a.id == b.id);
    Ok(build(kind, 2, &names, &entries))
}
