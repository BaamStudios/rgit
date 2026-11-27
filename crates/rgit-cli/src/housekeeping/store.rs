// SPDX-License-Identifier: MIT
//! The local object store as the housekeeping commands see it: loose
//! object files with their modification times, the packs of
//! `objects/pack` with their `.keep` state, writing a new pack and
//! removing redundant files (`gitrepository-layout(5)`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId};
use rgit_odb::ObjectDatabase;
use rgit_pack::Pack;
use rgit_pack::index_pack::{self as ip, IndexVersion, Input};
use rgit_pack::write::{self as pw, ListedObject, WriteOptions};

use super::secs;
use crate::Die;
use crate::messages::housekeeping as msg;

/// One file in a loose-object fan-out directory.
#[derive(Debug, Clone)]
pub struct FanEntry {
    /// The file.
    pub path: PathBuf,
    /// Its modification time (seconds).
    pub mtime: u64,
    /// The object it holds; `None` for other files (`tmp_obj_*`, ...).
    pub id: Option<ObjectId>,
    /// The file name starts with `tmp_`.
    pub temporary: bool,
}

/// The files of `objects/<fan>/`, sorted by name.
pub fn fan_entries(objects: &Path, fan: u8, hash: HashKind) -> Vec<FanEntry> {
    let hex = format!("{fan:02x}");
    let dir = objects.join(&hex);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let id = if name.len() == hash.hex_len() - 2
            && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            ObjectId::from_hex(hash, format!("{hex}{name}").as_bytes()).ok()
        } else {
            None
        };
        let meta = e.metadata();
        out.push(FanEntry {
            path: dir.join(name),
            mtime: meta.as_ref().map_or(0, |m| secs(m.modified())),
            id,
            temporary: name.starts_with("tmp_"),
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Every loose object of `objects` with its file and modification time.
pub fn loose_objects(objects: &Path, hash: HashKind) -> Vec<(ObjectId, PathBuf, u64)> {
    let mut out = Vec::new();
    for fan in 0..=255u8 {
        for e in fan_entries(objects, fan, hash) {
            if let Some(id) = e.id {
                out.push((id, e.path, e.mtime));
            }
        }
    }
    out
}

/// Files named `tmp_*` directly in `dir`, with their modification times.
pub fn temporary_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, u64)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("tmp_"))
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            let m = e.metadata();
            (e.path(), m.as_ref().map_or(0, |m| secs(m.modified())))
        })
        .collect();
    out.sort();
    out
}

/// A pack of the repository's own `objects/pack`.
#[derive(Clone)]
pub struct LocalPack {
    pub pack: Arc<Pack>,
    /// `pack-<hash>.pack`.
    pub file_name: String,
    /// A `.keep` file exists, or the pack was named with `--keep-pack`.
    pub keep: bool,
    /// Modification time of the `.pack` file.
    pub mtime: u64,
}

impl LocalPack {
    /// The path without extension.
    pub fn base(&self) -> PathBuf {
        self.pack.path().with_extension("")
    }
}

/// The local packs, newest first. `keep_names` are `--keep-pack`
/// arguments (`pack-<hash>.pack`, with or without the extension).
pub fn local_packs(odb: &ObjectDatabase, keep_names: &[String]) -> Vec<LocalPack> {
    let Some(store) = odb.packs() else {
        return Vec::new();
    };
    store.set().rescan();
    store
        .set()
        .packs()
        .into_iter()
        .map(|pack| {
            let path = pack.path().to_path_buf();
            let file_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let stem = file_name.strip_suffix(".pack").unwrap_or(&file_name);
            let named = keep_names
                .iter()
                .any(|k| k == &file_name || k.strip_suffix(".pack").unwrap_or(k) == stem);
            let mtime = secs(std::fs::metadata(&path).and_then(|m| m.modified()));
            LocalPack {
                keep: pack.is_keep() || named,
                pack,
                file_name,
                mtime,
            }
        })
        .collect()
}

/// The packed objects newer than `expire`: all objects of an ordinary
/// pack whose `.pack` file is newer, and the objects of a cruft pack whose
/// recorded time is newer.
pub fn recent_packed(packs: &[LocalPack], expire: u64) -> impl Iterator<Item = ObjectId> + '_ {
    packs.iter().flat_map(move |p| {
        let index = p.pack.index();
        let cruft = p.pack.is_cruft();
        let whole = !cruft && p.mtime > expire;
        (0..index.len()).filter_map(move |pos| {
            let recent = if cruft {
                p.pack
                    .mtimes()
                    .and_then(|m| m.get(pos))
                    .map_or(p.mtime, u64::from)
                    > expire
            } else {
                whole
            };
            recent.then(|| index.id_at(pos))
        })
    })
}

/// Remove a pack and the files that belong to it (index first, so that
/// readers never see an index without its pack). `.keep` files stay.
pub fn delete_pack(base: &Path) {
    for ext in ["idx", "rev", "bitmap", "mtimes", "promisor", "pack"] {
        let _ = std::fs::remove_file(base.with_extension(ext));
    }
}

/// Remove the leftovers of packs whose `.pack` file is gone: when an
/// `.idx` has no `.pack` next to it, every file with that base name goes
/// (`.idx`, `.keep`, `.rev`, ...). A `.pack` without `.idx` may still be
/// being written and stays, as does a lone `.keep`.
pub fn remove_pack_garbage(pack_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(pack_dir) else {
        return;
    };
    let names: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    for idx in names.iter().filter_map(|n| n.strip_suffix(".idx")) {
        if names.iter().any(|n| n.strip_suffix(".pack") == Some(idx)) {
            continue;
        }
        for n in names.iter().filter(|n| {
            n.strip_prefix(idx)
                .is_some_and(|rest| rest.starts_with('.') && !rest[1..].contains('.'))
        }) {
            let _ = std::fs::remove_file(pack_dir.join(n));
        }
    }
}

/// Remove the multi-pack index (`objects/pack/multi-pack-index`, its
/// incremental chain in `multi-pack-index.d/` and its bitmap and reverse
/// index files). It is only an index over the packs: after packs were
/// deleted it may name packs that are gone, which readers treat as
/// corruption, so it is dropped (gitformat-pack(5), "multi-pack-index").
pub fn remove_multi_pack_index(pack_dir: &Path) {
    let _ = std::fs::remove_file(pack_dir.join("multi-pack-index"));
    // The chain's directory stays, emptied, as with git.
    if let Ok(rd) = std::fs::read_dir(pack_dir.join("multi-pack-index.d")) {
        for e in rd.flatten() {
            let p = e.path();
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
    if let Ok(rd) = std::fs::read_dir(pack_dir) {
        for e in rd.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("multi-pack-index-")
                && (name.ends_with(".bitmap") || name.ends_with(".rev"))
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// How [`write_pack`] stores objects.
pub struct PackOptions {
    pub write: WriteOptions,
    /// Write a `.rev` file (`pack.writeReverseIndex`, default true).
    pub rev: bool,
    /// `.idx` version (`pack.indexVersion`).
    pub idx_version: IndexVersion,
    /// `core.sharedRepository` for the new files.
    pub shared: rgit_fs::SharedRepository,
}

impl PackOptions {
    /// Defaults from the configuration. Deltas use offsets unless
    /// `repack.useDeltaBaseOffset` is false.
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_config(config: &ConfigSet) -> Result<PackOptions, Die> {
        let mut write = WriteOptions::default();
        write.ofs_delta = config
            .bool("repack.usedeltabaseoffset")
            .ok()
            .flatten()
            .unwrap_or(true);
        for key in ["pack.compression", "core.compression"] {
            if let Ok(Some(v)) = config.int(key) {
                write.compression = match v {
                    -1 => None,
                    0..=9 => Some(v as u32),
                    _ => return Err(Die::Fatal(msg::bad_compression(v))),
                };
                break;
            }
        }
        if let Some(v) = config
            .ulong("core.bigfilethreshold")
            .map_err(|e| Die::Fatal(e.to_string()))?
        {
            write.big_file_threshold = v;
        }
        let rev = config
            .bool("pack.writereverseindex")
            .ok()
            .flatten()
            .unwrap_or(true);
        let idx_version = match config.int("pack.indexversion").ok().flatten() {
            Some(1) => IndexVersion {
                version: 1,
                ..IndexVersion::default()
            },
            _ => IndexVersion::default(),
        };
        Ok(PackOptions {
            write,
            rev,
            idx_version,
            shared: crate::refs_support::shared_repository(config),
        })
    }
}

fn io_fatal(path: &Path, e: std::io::Error) -> Die {
    Die::Fatal(msg::cannot_write(
        &path.display().to_string(),
        &e.to_string(),
    ))
}

/// Write `objects` (in this order) as a new pack into `pack_dir` with its
/// `.idx` (and `.rev`). Returns the pack's checksum (its name), or `None`
/// when there was nothing to write. A pack with the same name that exists
/// already is kept as it is.
pub fn write_pack(
    odb: &ObjectDatabase,
    pack_dir: &Path,
    objects: &[ListedObject],
    opts: &PackOptions,
) -> Result<Option<ObjectId>, Die> {
    Ok(write_packs(odb, pack_dir, objects, opts, None)?
        .into_iter()
        .next())
}

/// [`write_pack`] with a size limit (`--max-pack-size`,
/// `pack.packSizeLimit`): the objects may go into several packs, each at
/// most `max_size` bytes unless one object alone is larger. Returns the
/// names of the packs in the order they were written.
pub fn write_packs(
    odb: &ObjectDatabase,
    pack_dir: &Path,
    objects: &[ListedObject],
    opts: &PackOptions,
    max_size: Option<u64>,
) -> Result<Vec<ObjectId>, Die> {
    if objects.is_empty() {
        return Ok(Vec::new());
    }
    let hash = odb.hash_kind();
    std::fs::create_dir_all(pack_dir).map_err(|e| io_fatal(pack_dir, e))?;
    let prefix = format!(".tmp-{}-pack-", std::process::id());
    let packs: Vec<Arc<Pack>> = odb.packs().map(|p| p.set().packs()).unwrap_or_default();
    let mut temps: Vec<rgit_fs::TempFile> = Vec::new();
    match max_size {
        None => {
            let mut tmp = rgit_fs::TempFile::with_mode(pack_dir, &prefix, 0o444)
                .map_err(|e| io_fatal(pack_dir, e))?;
            let tmp_path = tmp.path().to_path_buf();
            {
                let out = std::io::BufWriter::with_capacity(1 << 16, tmp.file_mut());
                let (_summary, mut out) =
                    pw::write_pack(out, hash, objects, odb, &packs, &opts.write)
                        .map_err(|e| Die::Fatal(e.to_string()))?;
                out.flush().map_err(|e| io_fatal(&tmp_path, e))?;
            }
            temps.push(tmp);
        }
        Some(limit) => {
            let mut open = || -> std::io::Result<std::fs::File> {
                let mut tmp = rgit_fs::TempFile::with_mode(pack_dir, &prefix, 0o444)?;
                let file = tmp.file_mut().try_clone()?;
                temps.push(tmp);
                Ok(file)
            };
            let written =
                pw::write_pack_files(&mut open, hash, objects, odb, &packs, &opts.write, limit)
                    .map_err(|e| Die::Fatal(e.to_string()))?;
            for (_summary, file) in written {
                file.sync_all().map_err(|e| io_fatal(pack_dir, e))?;
            }
        }
    }
    let mut names = Vec::new();
    for tmp in temps {
        names.push(install_pack(tmp, pack_dir, hash, opts)?);
    }
    Ok(names)
}

/// Index the written temporary pack `tmp` and move it into place with its
/// `.idx` (and `.rev`).
fn install_pack(
    tmp: rgit_fs::TempFile,
    pack_dir: &Path,
    hash: HashKind,
    opts: &PackOptions,
) -> Result<ObjectId, Die> {
    let tmp_path = tmp.path().to_path_buf();
    let mut ip_opts = ip::Options::new(hash);
    // Large blobs are hashed while streaming, as git does (GIT_ALLOC_LIMIT).
    ip_opts.big_file_threshold = opts.write.big_file_threshold;
    let indexed = ip::index_pack(Input::File(&tmp_path), &ip_opts, None, None, None)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let checksum = indexed.checksum();
    let base = pack_dir.join(format!("pack-{}", checksum.to_hex()));
    let with = |ext: &str| base.with_extension(ext);
    if with("pack").exists() && with("idx").exists() {
        // The same objects in the same order: nothing to replace, but a
        // missing `.rev` is added (`pack.writeReverseIndex`).
        drop(tmp);
        if opts.rev && !with("rev").exists() {
            let bytes = indexed.rev_bytes().map_err(|e| Die::Fatal(e.to_string()))?;
            write_file(&with("rev"), "tmp_rev_", &bytes, opts.shared)?;
        }
        return Ok(checksum);
    }
    share(&tmp_path, opts.shared);
    tmp.persist(with("pack"))
        .map_err(|e| io_fatal(&with("pack"), e))?;
    if opts.rev {
        let bytes = indexed.rev_bytes().map_err(|e| Die::Fatal(e.to_string()))?;
        write_file(&with("rev"), "tmp_rev_", &bytes, opts.shared)?;
    }
    let bytes = indexed
        .index_bytes(opts.idx_version)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    write_file(&with("idx"), "tmp_idx_", &bytes, opts.shared)?;
    Ok(checksum)
}

fn write_file(
    dest: &Path,
    prefix: &str,
    data: &[u8],
    shared: rgit_fs::SharedRepository,
) -> Result<(), Die> {
    let dir = dest.parent().unwrap_or(Path::new("."));
    let mut tmp = rgit_fs::TempFile::with_mode(dir, prefix, 0o444).map_err(|e| io_fatal(dir, e))?;
    tmp.file_mut()
        .write_all(data)
        .map_err(|e| io_fatal(dest, e))?;
    share(tmp.path(), shared);
    tmp.persist(dest).map_err(|e| io_fatal(dest, e))
}

/// Apply `core.sharedRepository` to a new file; a read-only file stays
/// read-only.
pub fn share(path: &Path, shared: rgit_fs::SharedRepository) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let before = std::fs::metadata(path).map_or(0o644, |m| m.permissions().mode());
        let _ = shared.apply(path);
        if before & 0o200 == 0 {
            if let Ok(m) = std::fs::metadata(path) {
                let mode = m.permissions().mode() & !0o222;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (path, shared);
}

/// Write `id` as a loose object with modification time `mtime` (seconds),
/// unless a loose copy exists already.
pub fn loosen(odb: &ObjectDatabase, id: &ObjectId, mtime: u64) -> Result<(), Die> {
    let loose = odb.loose();
    if loose.contains(id) {
        return Ok(());
    }
    let big = odb
        .header(id)
        .ok()
        .flatten()
        .is_some_and(|h| odb.is_big(h.size));
    if big {
        // Large objects are copied as a stream.
        let mut r = odb
            .stream(id)
            .map_err(|e| Die::Fatal(e.to_string()))?
            .ok_or_else(|| Die::Fatal(msg::unable_to_read(&id.to_hex())))?;
        let (kind, size) = (r.kind(), r.size());
        loose
            .write_stream(kind, size, &mut r)
            .map_err(|e| Die::Fatal(e.to_string()))?;
    } else {
        let o = odb
            .read(id)
            .map_err(|e| Die::Fatal(e.to_string()))?
            .ok_or_else(|| Die::Fatal(msg::unable_to_read(&id.to_hex())))?;
        loose
            .write(o.kind, &o.data)
            .map_err(|e| Die::Fatal(e.to_string()))?;
    }
    let path = loose.path_of(id);
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
    let _ = rgit_fs::set_modified(&path, when);
    Ok(())
}

/// `git prune-packed`: remove loose objects that a local pack also holds.
/// With `dry_run` only print `rm -f <path>` for each. Empty fan-out
/// directories are removed. `display` is how `objects` is shown.
pub fn prune_packed(
    objects: &Path,
    display: &Path,
    hash: HashKind,
    packs: &[LocalPack],
    dry_run: bool,
) -> Result<(), Die> {
    let mut out = std::io::stdout().lock();
    for fan in 0..=255u8 {
        let entries = fan_entries(objects, fan, hash);
        if entries.is_empty() {
            continue;
        }
        for e in &entries {
            let Some(id) = e.id else { continue };
            if !packs.iter().any(|p| p.pack.contains(&id)) {
                continue;
            }
            if dry_run {
                let shown = display
                    .join(format!("{fan:02x}"))
                    .join(e.path.file_name().unwrap_or_default());
                let _ = writeln!(out, "rm -f {}", shown.display());
            } else {
                let _ = std::fs::remove_file(&e.path);
            }
        }
        if !dry_run {
            // Fails unless the directory is empty.
            let _ = std::fs::remove_dir(objects.join(format!("{fan:02x}")));
        }
    }
    Ok(())
}
