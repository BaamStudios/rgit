// SPDX-License-Identifier: MIT
//! `git pack-objects` (Documentation/git-pack-objects.adoc): packs from an
//! object list or from `--revs`, to standard output or to
//! `<base-name>-<hash>.{pack,idx,rev}`, with delta search (`--window`,
//! `--depth`, `pack.*`), reuse of existing deltas and objects, and thin
//! packs (`--thin`). See `rgit_pack::write`.
//!
//! Progress: `Enumerating objects`, `Counting objects`, `Compressing
//! objects`, `Writing objects` (`--all-progress`, or `<base-name>`
//! output), the `Total` line.
//!
//! Selection: `--include-tag`, `--reflog`, `--indexed-objects`, and
//! leaving out objects with `--local`, `--incremental`, `--unpacked`,
//! `--honor-pack-keep`, `--keep-pack=<name>`.
//!
//! Not yet: `--shallow`, `--stdin-packs`, the `delta` attribute, filters,
//! bitmaps.

use std::io::{BufRead, BufWriter, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rgit_config::ConfigSet;
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{ObjectDatabase, PrefixMatch};
use rgit_pack::index_pack::IndexVersion;
use rgit_pack::write::{self as pw, ListedObject, Tip, WriteOptions};
use rgit_refs::files::FilesStore;
use rgit_refs::name::{RefNameFlags, is_valid_refname};
use rgit_refs::{IterItem, IterOptions, MAX_SYMREF_DEPTH, RefStore, Target};

use crate::messages::pack_objects as msg;
use crate::messages::push as push_msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // -1: not given (a terminal shows progress), 0: none, 1: no writing
    // meter with `--stdout`, 2: all meters.
    let progress = Cell::new(-1);
    let all_progress_implied = Cell::new(false);
    let window = Cell::new(i32::MIN);
    let window_memory = Cell::new(u64::MAX);
    let depth = Cell::new(i32::MIN);
    let reuse_delta = Cell::new(true);
    let reuse_object = Cell::new(true);
    let ofs_delta = Cell::new(false);
    let threads = Cell::new(i32::MIN);
    let index_version: RefCell<Option<String>> = RefCell::new(None);
    let max_pack_size = Cell::new(u64::MAX);
    let non_empty = Cell::new(false);
    let revs = Cell::new(false);
    let all = Cell::new(false);
    let stdout = Cell::new(false);
    let sparse = Cell::new(-1);
    let thin = Cell::new(false);
    let include_tag = Cell::new(false);
    let reflog = Cell::new(false);
    let indexed_objects = Cell::new(false);
    let incremental = Cell::new(false);
    let unpacked = Cell::new(false);
    let honor_pack_keep = Cell::new(false);
    let keep_pack: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let keep_true_parents = Cell::new(false);
    let local = Cell::new(false);
    let path_walk = Cell::new(false);
    let use_bitmap = Cell::new(false);
    // 0: no bitmap, 1: `--write-bitmap-index`, 2: `--write-bitmap-index-quiet`.
    let write_bitmap = Cell::new(0);
    let name_hash_version = Cell::new(1);
    let compression = Cell::new(i32::MIN);
    // Options that change what is packed or written, not implemented yet:
    // remember the first one given and refuse to run.
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
        msg::USAGE,
        vec![
            Opt::set_int('q', "quiet", &progress, msg::H_QUIET, 0),
            Opt::set_int(None, "progress", &progress, msg::H_PROGRESS, 1),
            Opt::set_int(None, "all-progress", &progress, msg::H_ALL_PROGRESS, 2),
            Opt::flag(
                None,
                "all-progress-implied",
                &all_progress_implied,
                msg::H_ALL_PROGRESS_IMPLIED,
            ),
            Opt::string(
                None,
                "index-version",
                &index_version,
                "<version>[,<offset>]",
                msg::H_INDEX_VERSION,
            )
            .literal_argh()
            .noneg(),
            Opt::unsigned(None, "max-pack-size", &max_pack_size, msg::H_MAX_PACK_SIZE),
            Opt::flag(None, "local", &local, msg::H_LOCAL),
            Opt::flag(None, "incremental", &incremental, msg::H_INCREMENTAL),
            Opt::integer(None, "window", &window, msg::H_WINDOW),
            Opt::unsigned(None, "window-memory", &window_memory, msg::H_WINDOW_MEMORY),
            Opt::integer(None, "depth", &depth, msg::H_DEPTH),
            Opt::flag(None, "reuse-delta", &reuse_delta, msg::H_REUSE_DELTA),
            Opt::flag(None, "reuse-object", &reuse_object, msg::H_REUSE_OBJECT),
            Opt::flag(None, "delta-base-offset", &ofs_delta, msg::H_DELTA_BASE_OFFSET),
            Opt::integer(None, "threads", &threads, msg::H_THREADS),
            Opt::flag(None, "non-empty", &non_empty, msg::H_NON_EMPTY),
            Opt::flag(None, "revs", &revs, msg::H_REVS),
            Opt::flag(None, "unpacked", &unpacked, msg::H_UNPACKED).noneg(),
            Opt::flag(None, "all", &all, msg::H_ALL).noneg(),
            Opt::flag(None, "reflog", &reflog, msg::H_REFLOG).noneg(),
            Opt::flag(None, "indexed-objects", &indexed_objects, msg::H_INDEXED_OBJECTS).noneg(),
            Opt::callback(
                None,
                "stdin-packs",
                Some("mode"),
                msg::H_STDIN_PACKS,
                reject("stdin-packs"),
            )
            .optional(None),
            Opt::flag(None, "stdout", &stdout, msg::H_STDOUT),
            Opt::flag(None, "include-tag", &include_tag, msg::H_INCLUDE_TAG),
            Opt::callback(
                None,
                "keep-unreachable",
                None,
                msg::H_KEEP_UNREACHABLE,
                reject("keep-unreachable"),
            )
            .noarg(),
            Opt::callback(
                None,
                "pack-loose-unreachable",
                None,
                msg::H_PACK_LOOSE_UNREACHABLE,
                reject("pack-loose-unreachable"),
            )
            .noarg(),
            Opt::callback(
                None,
                "unpack-unreachable",
                Some("time"),
                msg::H_UNPACK_UNREACHABLE,
                reject("unpack-unreachable"),
            )
            .optional(None),
            Opt::callback(None, "cruft", None, msg::H_CRUFT, reject("cruft")).noarg(),
            Opt::callback(
                None,
                "cruft-expiration",
                Some("time"),
                msg::H_CRUFT_EXPIRATION,
                reject("cruft-expiration"),
            )
            .optional(None),
            Opt::bool(None, "sparse", &sparse, msg::H_SPARSE),
            Opt::flag(None, "thin", &thin, msg::H_THIN),
            Opt::flag(None, "path-walk", &path_walk, msg::H_PATH_WALK),
            Opt::callback(None, "shallow", None, msg::H_SHALLOW, reject("shallow")).noarg(),
            Opt::flag(None, "honor-pack-keep", &honor_pack_keep, msg::H_HONOR_PACK_KEEP),
            Opt::string_list(None, "keep-pack", &keep_pack, "name", msg::H_KEEP_PACK),
            Opt::integer(None, "compression", &compression, msg::H_COMPRESSION),
            // rgit's walk does not apply grafts: the true parents are kept
            // anyway.
            Opt::flag(None, "keep-true-parents", &keep_true_parents, msg::H_KEEP_TRUE_PARENTS),
            Opt::flag(None, "use-bitmap-index", &use_bitmap, msg::H_USE_BITMAP_INDEX),
            Opt::set_int(
                None,
                "write-bitmap-index",
                &write_bitmap,
                msg::H_WRITE_BITMAP_INDEX,
                1,
            ),
            Opt::set_int(
                None,
                "write-bitmap-index-quiet",
                &write_bitmap,
                msg::H_WRITE_BITMAP_INDEX,
                2,
            )
            .hidden(),
            Opt::callback(None, "filter", Some("args"), msg::H_FILTER, reject("filter")),
            Opt::callback(None, "missing", Some("action"), msg::H_MISSING, reject("missing"))
                .noneg(),
            Opt::callback(
                None,
                "exclude-promisor-objects",
                None,
                msg::H_EXCLUDE_PROMISOR_OBJECTS,
                reject("exclude-promisor-objects"),
            )
            .noarg(),
            Opt::callback(
                None,
                "exclude-promisor-objects-best-effort",
                None,
                msg::H_EXCLUDE_PROMISOR_OBJECTS_BEST_EFFORT,
                reject("exclude-promisor-objects-best-effort"),
            )
            .noarg(),
            Opt::callback(
                None,
                "delta-islands",
                None,
                msg::H_DELTA_ISLANDS,
                reject("delta-islands"),
            )
            .noarg(),
            Opt::callback(
                None,
                "uri-protocol",
                Some("protocol"),
                msg::H_URI_PROTOCOL,
                reject("uri-protocol"),
            ),
            Opt::integer(
                None,
                "name-hash-version",
                &name_hash_version,
                msg::H_NAME_HASH_VERSION,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    // A thin pack needs the edge of the walk: `--thin` reads revisions.
    if all.get() || thin.get() || reflog.get() || indexed_objects.get() {
        revs.set(true);
    }
    if stdout.get() != args.is_empty() || args.len() > 1 {
        return Err(parser.usage_error().into());
    }
    if let Some(opt) = unsupported.borrow().as_deref() {
        return Err(Die::Fatal(msg::not_supported(opt)));
    }
    if thin.get() && !stdout.get() {
        return Err(Die::Fatal(msg::THIN_NOT_INDEXABLE.into()));
    }
    let idx_version = match index_version.borrow().as_deref() {
        Some(v) => Some(parse_index_version(v)?),
        None => None,
    };

    let setup = repo::setup(ctx, Mode::Required)?;
    let hash = setup.format.object_format;
    let config = repo::config(ctx, &setup);
    let mut opts = config_options(&config)?;
    if compression.get() != i32::MIN {
        opts.compression = check_level(compression.get() as i64)?;
    }
    if window.get() != i32::MIN {
        opts.window = window.get().max(0) as usize;
    }
    if window_memory.get() != u64::MAX {
        opts.window_memory = window_memory.get();
    }
    if threads.get() != i32::MIN {
        opts.threads = threads.get().max(0) as usize;
    }
    if depth.get() != i32::MIN {
        opts.max_depth = depth.get().max(0) as u32;
    }
    if opts.max_depth > pw::MAX_DEPTH {
        eprintln!(
            "warning: {}",
            msg::depth_too_large(opts.max_depth, pw::MAX_DEPTH)
        );
        opts.max_depth = pw::MAX_DEPTH;
    }
    opts.ofs_delta = ofs_delta.get();
    let mut level = match progress.get() {
        -1 => i32::from(std::io::stderr().is_terminal()),
        v => v,
    };
    if all_progress_implied.get() && level > 0 {
        level = 2;
    }
    let show_progress = level > 0;
    opts.progress = pw::WriteProgress {
        compress: show_progress,
        write: level > 1 || (show_progress && !stdout.get()),
        throughput: level > 1 && stdout.get(),
    };
    opts.reuse_object = reuse_object.get();
    opts.reuse_delta = reuse_delta.get() && reuse_object.get();

    let odb = repo::odb_with_config(&setup, &config)?;
    let repo = Repo {
        source: &odb,
        refs: Refs::new(&setup),
        hash,
    };

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let (listed, bases): (Vec<ListedObject>, Vec<ListedObject>) = if revs.get() {
        let mut tips = Vec::new();
        if all.get() {
            for id in repo.refs.all() {
                tips.push(Tip { id, exclude: false });
            }
        }
        let mut extra = Vec::new();
        if reflog.get() {
            extra.extend(crate::housekeeping::reach::all_reflog_objects(&setup));
        }
        if indexed_objects.get() {
            extra.extend(crate::housekeeping::reach::all_index_objects(&setup));
        }
        for id in extra {
            if odb.contains(&id) {
                tips.push(Tip { id, exclude: false });
            }
        }
        read_revs(&mut input, &repo, &mut tips)?;
        let sparse = match sparse.get() {
            -1 => config.bool("pack.usesparse").ok().flatten().unwrap_or(true),
            v => v > 0,
        };
        let (listed, bases) = pw::enumerate_with_edges(&repo.source, hash, &tips, sparse)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        (listed, if thin.get() { bases } else { Vec::new() })
    } else {
        let (list, edges) = read_object_list(&mut input, hash)?;
        let ids: Vec<ObjectId> = list.iter().map(|(id, _)| *id).collect();
        let mut listed =
            pw::describe(&repo.source, hash, &ids).map_err(|e| Die::Fatal(e.to_string()))?;
        for (o, (_, name_hash)) in listed.iter_mut().zip(&list) {
            o.name_hash = *name_hash;
        }
        // Like git, edge objects serve as bases of a pack written to
        // standard output, which may make it thin.
        let bases = if stdout.get() {
            edge_objects(&repo, &edges)
        } else {
            Vec::new()
        };
        (listed, bases)
    };
    let mut listed = listed;
    let skip = Skip {
        local: local.get(),
        packed: incremental.get() || unpacked.get(),
        kept: honor_pack_keep.get(),
        keep_packs: keep_pack.borrow().clone(),
    };
    // Bitmaps are written only for packs of everything reachable.
    let mut bitmap = if stdout.get() || !all.get() {
        0
    } else {
        write_bitmap.get()
    };
    if skip.any() {
        let before = listed.len();
        skip.apply(&odb, &setup, &mut listed);
        if listed.len() != before && bitmap > 0 {
            if bitmap == 1 {
                eprintln!("warning: {}", msg::BITMAP_DISABLED_NOT_PACKED);
            }
            bitmap = 0;
        }
    }
    let walk_commits: Vec<ObjectId> = if bitmap > 0 {
        listed
            .iter()
            .filter(|o| o.kind == ObjectKind::Commit)
            .map(|o| o.id)
            .collect()
    } else {
        Vec::new()
    };
    if include_tag.get() {
        add_tags(&repo, &mut listed);
    }
    if show_progress {
        show_enumerated(&listed, &bases);
    }
    let ordered = pw::pack_order(&listed);
    if ordered.is_empty() && non_empty.get() {
        return Ok(0);
    }

    let packs = odb.packs().map(|p| p.set().packs()).unwrap_or_default();
    let summaries = if stdout.get() {
        let out = BufWriter::with_capacity(1 << 16, std::io::stdout().lock());
        let (summary, mut out) =
            pw::write_thin_pack(out, hash, &ordered, &bases, &repo.source, &packs, &opts)
                .map_err(|e| Die::Fatal(e.to_string()))?;
        out.flush().map_err(|e| Die::Fatal(e.to_string()))?;
        vec![summary]
    } else {
        // At least 1 MiB, as git-pack-objects(1) says.
        let limit = match max_pack_size.get() {
            u64::MAX => config
                .int64("pack.packsizelimit")
                .map_err(|e| Die::Fatal(e.to_string()))?
                .filter(|&v| v > 0)
                .map(|v| v as u64),
            0 => None,
            v => Some(v),
        }
        .map(|v| v.max(1 << 20));
        let files = PackFiles {
            base: PathBuf::from(&args[0]),
            hash,
            index_version: match idx_version {
                Some(v) => v,
                None => config_index_version(&config)?,
            },
            rev: config
                .bool("pack.writereverseindex")
                .ok()
                .flatten()
                .unwrap_or(true),
            max_pack_size: limit,
        };
        let summaries = files.write(&ordered, repo.source, &packs, &opts)?;
        if bitmap > 0 && summaries.len() == 1 {
            crate::bitmaps::write_for_pack(
                &setup,
                &config,
                &odb,
                &files.final_name(&summaries[0].checksum, "idx"),
                &walk_commits,
                show_progress,
            )?;
        }
        for s in &summaries {
            println!("{}", s.checksum);
        }
        summaries
    };
    if show_progress {
        let sum = |f: fn(&pw::PackSummary) -> u32| summaries.iter().map(f).sum::<u32>();
        eprintln!(
            "{}",
            msg::total(
                summaries.iter().map(|s| s.entries.len()).sum(),
                sum(|s| s.deltas),
                sum(|s| s.reused),
                sum(|s| s.reused_deltas)
            )
        );
    }
    Ok(0)
}

/// The `Enumerating objects` and `Counting objects` meters (also used by
/// push). Like git, both count the objects to pack plus the delta bases
/// the other side has under the path of a tree or blob that is packed.
pub(crate) fn show_enumerated(listed: &[ListedObject], bases: &[ListedObject]) {
    let paths: std::collections::HashSet<u32> = listed
        .iter()
        .filter(|o| matches!(o.kind, ObjectKind::Tree | ObjectKind::Blob))
        .map(|o| o.name_hash)
        .collect();
    let n = (listed.len()
        + bases
            .iter()
            .filter(|b| paths.contains(&b.name_hash))
            .count()) as u64;
    let mut meter = rgit_progress::Progress::start(push_msg::PROGRESS_ENUMERATING, 0, true);
    meter.set(n);
    meter.stop();
    let mut meter = rgit_progress::Progress::start(push_msg::PROGRESS_COUNTING, n, true);
    for i in 1..=n {
        meter.set(i);
    }
    meter.stop();
}

/// The pack settings from configuration: `pack.window`, `pack.depth`,
/// `pack.windowMemory`, `pack.threads`, `pack.compression` (else
/// `core.compression`) and `core.bigFileThreshold`. Also used by `push`.
pub(crate) fn config_options(config: &ConfigSet) -> Result<WriteOptions, Die> {
    let mut opts = WriteOptions {
        compression: config_compression(config)?,
        ..WriteOptions::default()
    };
    let int = |key: &str, wide: bool| -> Result<Option<i64>, Die> {
        let v = if wide {
            config.int64(key)
        } else {
            config.int(key)
        };
        v.map_err(|e| Die::Fatal(e.to_string()))
    };
    if let Some(v) = int("pack.window", false)? {
        opts.window = v.max(0) as usize;
    }
    if let Some(v) = int("pack.depth", false)? {
        opts.max_depth = v.max(0) as u32;
    }
    if let Some(v) = int("pack.windowmemory", true)? {
        opts.window_memory = v.max(0) as u64;
    }
    if let Some(v) = int("pack.threads", false)? {
        opts.threads = v.max(0) as usize;
    }
    if let Some(v) = int("core.bigfilethreshold", true)? {
        opts.big_file_threshold = v.max(0) as u64;
    }
    Ok(opts)
}

/// `--index-version=<version>[,<offset>]`.
fn parse_index_version(v: &str) -> Result<IndexVersion, Die> {
    let bad = || Die::Fatal(msg::bad_index_version(v));
    let (ver, off) = match v.split_once(',') {
        Some((a, b)) => (a, Some(b)),
        None => (v, None),
    };
    let version: u32 = ver.parse().map_err(|_| bad())?;
    if version > 2 {
        return Err(Die::Fatal(msg::unsupported_index_version(version as i64)));
    }
    let mut out = IndexVersion {
        version,
        ..IndexVersion::default()
    };
    if let Some(off) = off {
        let parsed = match off.strip_prefix("0x") {
            Some(h) => u64::from_str_radix(h, 16),
            None => off.parse::<u64>(),
        };
        out.large_offset_limit = parsed.map_err(|_| bad())?;
    }
    Ok(out)
}

/// `pack.indexVersion` (default 2).
fn config_index_version(config: &ConfigSet) -> Result<IndexVersion, Die> {
    let mut v = IndexVersion::default();
    if let Ok(Some(n)) = config.int("pack.indexversion") {
        if !(1..=2).contains(&n) {
            return Err(Die::Fatal(msg::bad_config_index_version(n)));
        }
        v.version = n as u32;
    }
    Ok(v)
}

/// Where and how `<base-name>` output is written.
struct PackFiles {
    base: PathBuf,
    hash: HashKind,
    index_version: IndexVersion,
    /// Also write a `.rev` file (`pack.writeReverseIndex`).
    rev: bool,
    /// Split into packs of at most this size (`--max-pack-size`,
    /// `pack.packSizeLimit`).
    max_pack_size: Option<u64>,
}

impl PackFiles {
    fn dir(&self) -> PathBuf {
        match self.base.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        }
    }

    fn final_name(&self, checksum: &ObjectId, ext: &str) -> PathBuf {
        let mut s = self.base.as_os_str().to_owned();
        s.push(format!("-{checksum}.{ext}"));
        PathBuf::from(s)
    }

    /// Write `<base>-<hash>.pack`, `.idx` and maybe `.rev`, read-only,
    /// through temporary files in the same directory; with a size limit
    /// possibly several packs.
    fn write(
        &self,
        objects: &[ListedObject],
        source: &ObjectDatabase,
        packs: &[std::sync::Arc<rgit_pack::Pack>],
        opts: &WriteOptions,
    ) -> Result<Vec<pw::PackSummary>, Die> {
        let dir = self.dir();
        let mut temps = TempFiles(Vec::new());
        let fatal = |e: pw::WriteError| Die::Fatal(e.to_string());
        let mut written: Vec<(pw::PackSummary, PathBuf)> = Vec::new();
        match self.max_pack_size {
            None => {
                let tmp_pack = dir.join(format!("tmp_pack_{}", std::process::id()));
                let file =
                    std::fs::File::create(&tmp_pack).map_err(|e| write_error(&tmp_pack, &e))?;
                temps.0.push((tmp_pack.clone(), "pack"));
                let out = BufWriter::with_capacity(1 << 16, file);
                let (summary, out) =
                    pw::write_pack(out, self.hash, objects, source, packs, opts).map_err(fatal)?;
                let file = out
                    .into_inner()
                    .map_err(|e| write_error(&tmp_pack, e.error()))?;
                file.sync_all().map_err(|e| write_error(&tmp_pack, &e))?;
                written.push((summary, tmp_pack));
            }
            Some(limit) => {
                let mut paths = Vec::new();
                let mut open = || {
                    let p = dir.join(format!("tmp_pack_{}_{}", std::process::id(), paths.len()));
                    let f = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&p)?;
                    paths.push(p);
                    Ok(f)
                };
                let result =
                    pw::write_pack_files(&mut open, self.hash, objects, source, packs, opts, limit);
                for p in &paths {
                    temps.0.push((p.clone(), "pack"));
                }
                for ((summary, file), path) in result.map_err(fatal)?.into_iter().zip(paths) {
                    file.sync_all().map_err(|e| write_error(&path, &e))?;
                    written.push((summary, path));
                }
            }
        }
        // Per pack: the pack first and the index last, as readers look for
        // the index.
        let mut summaries = Vec::new();
        for (n, (summary, tmp_pack)) in written.into_iter().enumerate() {
            let indexed = summary.indexed(self.hash);
            let mut files = vec![(tmp_pack, "pack")];
            if self.rev {
                let bytes = indexed.rev_bytes().map_err(|e| Die::Fatal(e.to_string()))?;
                let tmp = dir.join(format!("tmp_rev_{}_{n}", std::process::id()));
                temps.0.push((tmp.clone(), "rev"));
                std::fs::write(&tmp, bytes).map_err(|e| write_error(&tmp, &e))?;
                files.push((tmp, "rev"));
            }
            let bytes = indexed
                .index_bytes(self.index_version)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            let tmp = dir.join(format!("tmp_idx_{}_{n}", std::process::id()));
            temps.0.push((tmp.clone(), "idx"));
            std::fs::write(&tmp, bytes).map_err(|e| write_error(&tmp, &e))?;
            files.push((tmp, "idx"));
            for (tmp, ext) in &files {
                set_read_only(tmp);
                let dest = self.final_name(&summary.checksum, ext);
                std::fs::rename(tmp, &dest).map_err(|e| write_error(&dest, &e))?;
                temps.0.retain(|(p, _)| p != tmp);
            }
            summaries.push(summary);
        }
        Ok(summaries)
    }
}

fn write_error(path: &Path, e: &dyn std::fmt::Display) -> Die {
    Die::Fatal(msg::unable_to_write(&path.display().to_string(), &e.to_string()))
}

/// Temporary files, removed when dropped unless cleared.
struct TempFiles(Vec<(PathBuf, &'static str)>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for (p, _) in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
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

/// `pack.compression`, else `core.compression`; `-1` is zlib's default.
fn config_compression(config: &ConfigSet) -> Result<Option<u32>, Die> {
    for key in ["pack.compression", "core.compression"] {
        if let Ok(Some(v)) = config.int(key) {
            return check_level(v);
        }
    }
    Ok(None)
}

fn check_level(level: i64) -> Result<Option<u32>, Die> {
    match level {
        -1 => Ok(None),
        0..=9 => Ok(Some(level as u32)),
        _ => Err(Die::Fatal(msg::bad_compression(level))),
    }
}

fn read_line(input: &mut dyn BufRead) -> Result<Option<String>, Die> {
    let mut buf = Vec::new();
    match input.read_until(b'\n', &mut buf) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(String::from_utf8_lossy(&buf).into_owned())),
        Err(e) => Err(Die::Fatal(e.to_string())),
    }
}

/// The object list: one id per line, optionally followed by a space and a
/// path (a hint for the delta search), with the name hash of that path.
/// Lines `-<id>` name edge objects the receiver has (returned second): a
/// pack written to standard output may use them as delta bases.
#[allow(clippy::type_complexity)]
fn read_object_list(
    input: &mut dyn BufRead,
    hash: HashKind,
) -> Result<(Vec<(ObjectId, u32)>, Vec<ObjectId>), Die> {
    let mut list = Vec::new();
    let mut edges = Vec::new();
    let hex_len = hash.hex_len();
    while let Some(line) = read_line(input)? {
        if let Some(rest) = line.strip_prefix('-') {
            let id = rest
                .as_bytes()
                .get(..hex_len)
                .and_then(|h| ObjectId::from_hex(hash, h).ok());
            match id {
                Some(id) => edges.push(id),
                None => return Err(Die::Fatal(msg::expected_edge_id(&line))),
            }
            continue;
        }
        let hex = line.as_bytes().get(..hex_len);
        let rest = line.as_bytes().get(hex_len..).unwrap_or(b"");
        let id = hex.and_then(|h| ObjectId::from_hex(hash, h).ok());
        match id {
            Some(id) if rest.is_empty() || rest[0] == b' ' || rest[0] == b'\n' => {
                let path = rest.strip_prefix(b" ").unwrap_or(b"");
                let path = path.strip_suffix(b"\n").unwrap_or(path);
                list.push((id, pw::name_hash(path)));
            }
            _ => return Err(Die::Fatal(msg::expected_object_id(&line))),
        }
    }
    Ok((list, edges))
}

/// The objects below the edge objects of an object list (commits stand for
/// their tree): delta bases for a pack the receiver completes. Objects that
/// cannot be read are left out.
fn edge_objects(repo: &Repo<'_>, edges: &[ObjectId]) -> Vec<ListedObject> {
    let mut out = Vec::new();
    for id in edges {
        match pw::ObjectSource::kind(&repo.source, id) {
            Ok(Some(ObjectKind::Blob)) => out.push(ListedObject::new(*id, ObjectKind::Blob)),
            Ok(Some(_)) => {
                let Some(tree) = peel(repo, *id, Some(ObjectKind::Tree)) else {
                    continue;
                };
                let tips = [Tip {
                    id: tree,
                    exclude: true,
                }];
                if let Ok((_, objects)) = pw::enumerate_with_edges(&repo.source, repo.hash, &tips, false) {
                    out.extend(objects);
                }
            }
            _ => {}
        }
    }
    out
}

/// What resolving revisions needs.
struct Repo<'a> {
    source: &'a ObjectDatabase,
    refs: Refs,
    hash: HashKind,
}

/// Revision arguments, one per line: `<rev>`, `^<rev>`, `--not`. An empty
/// line ends the revisions.
fn read_revs(input: &mut dyn BufRead, repo: &Repo<'_>, tips: &mut Vec<Tip>) -> Result<(), Die> {
    let mut not = false;
    while let Some(line) = read_line(input)? {
        let line = line.strip_suffix('\n').unwrap_or(&line);
        if line.is_empty() {
            break;
        }
        if line == "--not" {
            not = !not;
            continue;
        }
        if line.starts_with('-') {
            return Err(Die::Fatal(msg::not_a_rev(line)));
        }
        let (exclude, name) = match line.strip_prefix('^') {
            Some(rest) => (!not, rest),
            None => (not, line),
        };
        let id = resolve(repo, name)?;
        tips.push(Tip { id, exclude });
    }
    Ok(())
}

/// Resolve a revision: `<name>` followed by any number of `~<n>`, `^<n>`,
/// `^{}`, `^{commit}` or `^{tree}` (`git-rev-parse(1)`, "SPECIFYING
/// REVISIONS").
fn resolve(repo: &Repo<'_>, rev: &str) -> Result<ObjectId, Die> {
    let split = rev.find(['~', '^']).unwrap_or(rev.len());
    let (name, mut suffix) = rev.split_at(split);
    let mut id = resolve_name(repo, name, rev)?;
    let bad = || Die::Fatal(msg::bad_revision(rev));
    while !suffix.is_empty() {
        let op = suffix.as_bytes()[0];
        let rest = &suffix[1..];
        if op == b'^' && rest.starts_with('{') {
            let end = rest.find('}').ok_or_else(bad)?;
            let want = match &rest[1..end] {
                "" => None,
                "commit" => Some(ObjectKind::Commit),
                "tree" => Some(ObjectKind::Tree),
                _ => return Err(Die::Fatal(msg::rev_not_supported(rev))),
            };
            id = peel(repo, id, want).ok_or_else(bad)?;
            suffix = &rest[end + 1..];
            continue;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let n: usize = if digits == 0 {
            1
        } else {
            rest[..digits].parse().map_err(|_| bad())?
        };
        suffix = &rest[digits..];
        id = peel(repo, id, Some(ObjectKind::Commit)).ok_or_else(bad)?;
        if op == b'^' {
            if n > 0 {
                let parents = parents(repo, &id).ok_or_else(bad)?;
                id = *parents.get(n - 1).ok_or_else(bad)?;
            }
        } else {
            for _ in 0..n {
                id = *parents(repo, &id).ok_or_else(bad)?.first().ok_or_else(bad)?;
            }
        }
    }
    Ok(id)
}

/// A full id, a ref name, or an unambiguous id prefix.
fn resolve_name(repo: &Repo<'_>, name: &str, rev: &str) -> Result<ObjectId, Die> {
    let hash = repo.hash;
    let is_hex = !name.is_empty() && name.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex && name.len() == hash.hex_len() {
        let id = ObjectId::from_hex(hash, name.as_bytes()).expect("hex checked");
        return match pw::ObjectSource::kind(&repo.source, &id) {
            Ok(Some(_)) => Ok(id),
            _ => Err(Die::Fatal(msg::bad_object(name))),
        };
    }
    if let Some(id) = repo.refs.dwim(name) {
        return Ok(id);
    }
    if is_hex && name.len() >= 4 && name.len() < hash.hex_len() {
        if let Some(id) = unique_prefix(repo.source, name) {
            return Ok(id);
        }
    }
    if rev.contains([':', '@', '{']) {
        return Err(Die::Fatal(msg::rev_not_supported(rev)));
    }
    Err(Die::Fatal(msg::bad_revision(rev)))
}

/// Follow tags (and from a commit to its tree when `want` is a tree)
/// until an object of type `want` (any non-tag for `None`).
fn peel(repo: &Repo<'_>, mut id: ObjectId, want: Option<ObjectKind>) -> Option<ObjectId> {
    for _ in 0..64 {
        let o = pw::ObjectSource::read(&repo.source, &id).ok()??;
        if Some(o.kind) == want || (want.is_none() && o.kind != ObjectKind::Tag) {
            return Some(id);
        }
        id = match o.kind {
            ObjectKind::Tag => rgit_object::Tag::parse(repo.hash, &o.data)
                .ok()?
                .object()
                .ok()?,
            ObjectKind::Commit if want == Some(ObjectKind::Tree) => {
                rgit_object::Commit::parse(repo.hash, &o.data)
                    .ok()?
                    .tree()
                    .ok()?
            }
            _ => return None,
        };
    }
    None
}

fn parents(repo: &Repo<'_>, id: &ObjectId) -> Option<Vec<ObjectId>> {
    let o = pw::ObjectSource::read(&repo.source, id).ok()??;
    rgit_object::Commit::parse(repo.hash, &o.data)
        .ok()?
        .parents()
        .ok()
}

/// Objects left out of the pack although listed.
struct Skip {
    /// `--local`: objects only found in alternate object directories.
    local: bool,
    /// `--incremental`, `--unpacked`: objects already in a pack.
    packed: bool,
    /// `--honor-pack-keep`: objects in a local pack with a `.keep` file.
    kept: bool,
    /// `--keep-pack=<name>`: objects in these packs (file names).
    keep_packs: Vec<String>,
}

impl Skip {
    fn any(&self) -> bool {
        self.local || self.packed || self.kept || !self.keep_packs.is_empty()
    }

    fn apply(&self, odb: &ObjectDatabase, setup: &rgit_repo::Setup, listed: &mut Vec<ListedObject>) {
        let local_packs = odb.packs().map(|p| p.set().packs()).unwrap_or_default();
        let objects = rgit_odb::objects_dir(&setup.cwd.join(&setup.common_dir));
        let alternates = if self.packed || self.local {
            rgit_odb::resolve_alternates(&objects)
        } else {
            Vec::new()
        };
        let alt_packs: Vec<std::sync::Arc<rgit_pack::Pack>> = alternates
                .iter()
                .flat_map(|alt| rgit_pack::PackSet::open(alt.join("pack"), odb.hash_kind()).packs())
                .collect();
        let in_alternate = |id: &ObjectId| {
            let hex = id.to_hex();
            alt_packs.iter().any(|p| p.contains(id))
                || alternates
                    .iter()
                    .any(|alt| alt.join(&hex[..2]).join(&hex[2..]).is_file())
        };
        let file_name = |p: &rgit_pack::Pack| {
            p.path()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut dropped: Vec<std::sync::Arc<rgit_pack::Pack>> = Vec::new();
        for p in &local_packs {
            let name = file_name(p);
            let kept = self.kept && p.path().with_extension("keep").exists();
            if kept || self.keep_packs.contains(&name) {
                dropped.push(p.clone());
            }
        }
        listed.retain(|o| {
            // Like git, an object the alternates have is borrowed even when
            // there is a local copy.
            if self.local && in_alternate(&o.id) {
                return false;
            }
            if self.packed
                && (local_packs.iter().any(|p| p.contains(&o.id))
                    || alt_packs.iter().any(|p| p.contains(&o.id)))
            {
                return false;
            }
            !dropped.iter().any(|p| p.contains(&o.id))
        });
    }
}

/// `--include-tag`: annotated tags under `refs/tags/` whose target
/// (through any further tags) is packed, with the tags in between.
fn add_tags(repo: &Repo<'_>, listed: &mut Vec<ListedObject>) {
    let mut have: std::collections::HashSet<ObjectId> = listed.iter().map(|o| o.id).collect();
    let Ok(items) = repo.refs.common.iter(&IterOptions::prefix("refs/tags/")) else {
        return;
    };
    for item in items {
        let IterItem::Ref(r) = item else { continue };
        let Some(mut id) = r.oid else { continue };
        let mut chain = Vec::new();
        let peeled = loop {
            if chain.len() > 64 {
                break None;
            }
            let Ok(Some(o)) = pw::ObjectSource::read(&repo.source, &id) else {
                break None;
            };
            if o.kind != ObjectKind::Tag {
                break Some(id);
            }
            chain.push(id);
            match rgit_object::Tag::parse(repo.hash, &o.data).and_then(|t| t.object()) {
                Ok(next) => id = next,
                Err(_) => break None,
            }
        };
        if chain.is_empty() || !peeled.is_some_and(|p| have.contains(&p)) {
            continue;
        }
        for t in chain {
            if have.insert(t) {
                listed.push(ListedObject::new(t, ObjectKind::Tag));
            }
        }
    }
}

/// The only object whose id starts with `hex`.
fn unique_prefix(odb: &ObjectDatabase, hex: &str) -> Option<ObjectId> {
    let prefix = Prefix::from_hex(odb.hash_kind(), hex.as_bytes()).ok()?;
    match odb.resolve_prefix(&prefix) {
        Ok(PrefixMatch::Unique(id)) => Some(id),
        _ => None,
    }
}

/// References through `rgit_refs`: per-worktree names (`HEAD`, ...) from
/// the git directory, the rest from the common directory.
struct Refs {
    worktree: FilesStore,
    common: FilesStore,
}

impl Refs {
    fn new(setup: &rgit_repo::Setup) -> Refs {
        let kind = setup.format.object_format;
        Refs {
            worktree: FilesStore::new(setup.absolute_git_dir(), kind),
            common: FilesStore::new(setup.cwd.join(&setup.common_dir), kind),
        }
    }

    fn store(&self, name: &str) -> &FilesStore {
        let per_worktree = !name.starts_with("refs/")
            || name.starts_with("refs/bisect/")
            || name.starts_with("refs/worktree/")
            || name.starts_with("refs/rewritten/");
        if per_worktree {
            &self.worktree
        } else {
            &self.common
        }
    }

    /// Follow `name` (and symbolic refs) to an object id.
    fn resolve(&self, name: &str) -> Option<ObjectId> {
        if !is_valid_refname(name.as_bytes(), RefNameFlags::ONELEVEL) {
            return None;
        }
        let mut name = name.to_string();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.store(&name).read_raw(&name).ok()?? {
                Target::Object(id) => return Some(id),
                Target::Symbolic(next) => name = next,
            }
        }
        None
    }

    /// `<refname>` lookup order of `git-rev-parse(1)`.
    fn dwim(&self, name: &str) -> Option<ObjectId> {
        let root = !name.is_empty() && name.bytes().all(|b| b.is_ascii_uppercase() || b == b'_');
        let candidates = [
            name.to_string(),
            format!("refs/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ];
        candidates
            .iter()
            .filter(|c| c.starts_with("refs/") || root)
            .find_map(|c| self.resolve(c))
    }

    /// `HEAD` and every reference under `refs/` (`--all`).
    fn all(&self) -> Vec<ObjectId> {
        let mut out: Vec<ObjectId> = self.resolve("HEAD").into_iter().collect();
        if let Ok(items) = self.common.iter(&IterOptions::default()) {
            for item in items {
                if let IterItem::Ref(r) = item {
                    out.extend(r.oid);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_list_lines() {
        let mut input: &[u8] = b"0123456789012345678901234567890123456789\n\
            1123456789012345678901234567890123456789 some/path\n\
            -2123456789012345678901234567890123456789\n";
        let (list, edges) = read_object_list(&mut input, HashKind::Sha1).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(edges.len(), 1);
        assert_eq!(list[0].1, 0);
        assert_eq!(list[1].1, pw::name_hash(b"some/path"));
        let mut bad: &[u8] = b"HEAD\n";
        assert!(read_object_list(&mut bad, HashKind::Sha1).is_err());
    }

    #[test]
    fn levels() {
        assert_eq!(check_level(-1).unwrap(), None);
        assert_eq!(check_level(9).unwrap(), Some(9));
        assert!(check_level(10).is_err());
    }

}
