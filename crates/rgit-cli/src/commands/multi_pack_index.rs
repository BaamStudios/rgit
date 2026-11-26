// SPDX-License-Identifier: MIT
//! `git multi-pack-index` (Documentation/git-multi-pack-index.adoc,
//! Documentation/technical/multi-pack-index.adoc): `write` (also
//! `--incremental` chains, `--stdin-packs`, `--preferred-pack`), `verify`,
//! `expire`, `repack` and `compact`. The file format is in
//! `rgit_pack::midx`.
//!
//! Multi-pack bitmaps (#23) are not written: `--bitmap` and
//! `--refs-snapshot` are accepted, and the MIDX is written without a bitmap.

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_pack::midx::{self, MidxChain, write as mw};

use crate::housekeeping::store;
use crate::messages::multi_pack_index as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

const SUBCOMMANDS: [&str; 5] = ["write", "compact", "verify", "expire", "repack"];
const ALL_USAGE: [&str; 5] = [
    msg::USAGE_WRITE,
    msg::USAGE_COMPACT,
    msg::USAGE_VERIFY,
    msg::USAGE_EXPIRE,
    msg::USAGE_REPACK,
];

/// Options shared by all subcommands.
struct Common {
    object_dir: RefCell<Option<String>>,
    progress: Cell<i32>,
}

impl Common {
    fn new() -> Common {
        Common {
            object_dir: RefCell::new(None),
            progress: Cell::new(-1),
        }
    }

    fn opts(&self) -> Vec<Opt<'_>> {
        vec![
            Opt::string(None, "object-dir", &self.object_dir, "directory", msg::H_OBJECT_DIR),
            Opt::bool(None, "progress", &self.progress, msg::H_PROGRESS),
        ]
    }

    fn show_progress(&self) -> bool {
        match self.progress.get() {
            -1 => rgit_progress::stderr_is_terminal(),
            v => v != 0,
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let common = Common::new();
    let mut opts = common.opts();
    opts.extend(SUBCOMMANDS.iter().map(|s| Opt::subcommand(s)));
    let mut parser = Parser::new(&ALL_USAGE, opts);
    let parsed = parser.parse(argv)?;
    drop(parser);
    let sub = parsed.subcommand.unwrap_or_default();
    let args = &parsed.args;
    match sub.as_str() {
        "write" => write(ctx, &common, args),
        "compact" => compact(ctx, &common, args),
        "verify" => verify(ctx, &common, args),
        "expire" => expire(ctx, &common, args),
        _ => repack(ctx, &common, args),
    }
}

/// The object directory to work on and how messages name it.
struct Target {
    objects: PathBuf,
    display: String,
    kind: HashKind,
}

impl Target {
    fn pack_dir(&self) -> PathBuf {
        self.objects.join("pack")
    }

    fn pack_display(&self) -> String {
        format!("{}/pack", self.display)
    }
}

fn target(ctx: &Context, common: &Common) -> Result<(rgit_repo::Setup, Target), Die> {
    let setup = repo::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let own = rgit_odb::objects_dir(&setup.common_dir);
    let Some(given) = common.object_dir.borrow().clone() else {
        let display = own.display().to_string();
        return Ok((
            setup,
            Target {
                objects: own,
                display,
                kind,
            },
        ));
    };
    let path = {
        let p = Path::new(&given);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            setup.original_cwd.join(p)
        }
    };
    let canon = |p: &Path| std::fs::canonicalize(p).ok();
    let want = canon(&path);
    let mut candidates = vec![own.clone()];
    candidates.extend(rgit_odb::resolve_alternates(&own));
    let ok = want.is_some() && candidates.iter().any(|c| canon(c) == want);
    if !ok {
        let shown = want.unwrap_or_else(|| normalize(&path));
        return Err(Die::Fatal(msg::not_an_alternate(&shown.display().to_string())));
    }
    let display = given.trim_end_matches('/').to_owned();
    Ok((
        setup,
        Target {
            objects: path,
            display,
            kind,
        },
    ))
}

/// Lexically normalized absolute path (for messages about missing
/// directories).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn midx_version(config: &rgit_config::ConfigSet) -> u8 {
    match config.int("midx.version").ok().flatten() {
        Some(2) => 2,
        _ => 1,
    }
}

fn map_write_error(e: mw::WriteError) -> Die {
    match e {
        mw::WriteError::Error(m) => Die::Error(m, 255),
        mw::WriteError::Fatal(m) => Die::Fatal(m),
    }
}

fn write(ctx: &mut Context, common: &Common, argv: &[String]) -> CmdResult {
    let preferred: RefCell<Option<String>> = RefCell::new(None);
    let bitmap = Cell::new(0);
    let base: RefCell<Option<String>> = RefCell::new(None);
    let incremental = Cell::new(0);
    let chain_file = Cell::new(1);
    let stdin_packs = Cell::new(0);
    let refs_snapshot: RefCell<Option<String>> = RefCell::new(None);
    let mut opts = common.opts();
    opts.extend([
        Opt::string(None, "preferred-pack", &preferred, "preferred-pack", msg::H_PREFERRED),
        Opt::bool(None, "bitmap", &bitmap, msg::H_BITMAP),
        Opt::string(None, "base", &base, "checksum", msg::H_BASE),
        Opt::bool(None, "incremental", &incremental, msg::H_INCREMENTAL),
        Opt::bool(None, "write-chain-file", &chain_file, msg::H_CHAIN_FILE),
        Opt::bool(None, "stdin-packs", &stdin_packs, msg::H_STDIN_PACKS),
        Opt::filename(None, "refs-snapshot", &refs_snapshot, msg::H_REFS_SNAPSHOT),
    ]);
    let mut parser = Parser::new(&[msg::USAGE_WRITE], opts);
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    if chain_file.get() == 0 && incremental.get() == 0 {
        return Err(parser
            .error_with_usage(msg::NO_CHAIN_WITHOUT_INCREMENTAL)
            .into());
    }
    if base.borrow().is_some() && chain_file.get() != 0 {
        return Err(parser.error_with_usage(msg::BASE_WITHOUT_NO_CHAIN).into());
    }
    drop(parser);
    let (setup, target) = target(ctx, common)?;
    let config = repo::load_config(ctx, &setup)?;
    let only = if stdin_packs.get() != 0 {
        let mut set = BTreeSet::new();
        for line in std::io::stdin().lock().lines() {
            let line = line.map_err(|e| Die::Fatal(e.to_string()))?;
            if !line.is_empty() {
                set.insert(line);
            }
        }
        Some(set)
    } else {
        None
    };
    let opts = mw::WriteOptions {
        version: midx_version(&config),
        preferred: preferred.take(),
        only,
        bitmap: bitmap.get() != 0,
        incremental: incremental.get() != 0,
        no_chain_file: chain_file.get() == 0,
        base: base.take().map(|b| (b != "none" && !b.is_empty()).then_some(b)),
        progress: common.show_progress(),
        drop: BTreeSet::new(),
        write_rev: std::env::var("GIT_TEST_MIDX_WRITE_REV").is_ok_and(|v| v == "1" || v == "true"),
    };
    let snapshot = match refs_snapshot.take() {
        Some(p) => Some(crate::bitmaps::read_refs_snapshot(
            std::path::Path::new(&p),
            target.kind,
        )?),
        None => None,
    };
    let hashes = if opts.bitmap && !opts.incremental {
        crate::bitmaps::existing_name_hashes(&target.pack_dir(), target.kind)
    } else {
        Default::default()
    };
    let outcome = mw::write(&target.pack_dir(), &target.pack_display(), target.kind, &opts)
        .map_err(map_write_error)?;
    if opts.bitmap
        && !opts.incremental
        && outcome.written.is_some()
    {
        crate::bitmaps::write_for_midx(
            ctx,
            &setup,
            &config,
            &target.pack_dir(),
            &hashes,
            snapshot.as_deref(),
        )?;
    }
    if opts.no_chain_file
        && let Some(sum) = outcome.written
    {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", sum.to_hex());
    }
    Ok(0)
}

fn verify(ctx: &mut Context, common: &Common, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_VERIFY], common.opts());
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (_setup, target) = target(ctx, common)?;
    let report = midx::verify::verify(&target.pack_dir(), target.kind, common.show_progress())
        .map_err(|e| Die::Fatal(e.0))?;
    Ok(i32::from(report.failed()))
}

/// Load the stand-alone MIDX of `target` for `expire` and `repack`;
/// `None` when there is none.
fn load_single(target: &Target, incremental_msg: &str) -> Result<Option<MidxChain>, Die> {
    let mut errors = Vec::new();
    let chain = MidxChain::open(&target.pack_dir(), target.kind, &mut errors);
    for e in &errors {
        eprintln!("error: {e}");
    }
    let chain = chain.map_err(|e| Die::Fatal(e.0))?;
    match chain {
        Some(c) if c.is_incremental() => Err(Die::Fatal(incremental_msg.to_owned())),
        other => Ok(other),
    }
}

/// Per pack-int-id: the number of objects the MIDX takes from that pack.
fn referenced_counts(m: &midx::MultiPackIndex, progress: bool) -> Result<Vec<u64>, Die> {
    let mut counts = vec![0u64; m.num_packs() as usize];
    let mut p = rgit_progress::Progress::start(
        msg::PROGRESS_COUNTING_REFERENCED,
        u64::from(m.len()),
        progress,
    );
    for i in 0..m.len() {
        let pack = m.pack_id(i).map_err(Die::Fatal)?;
        counts[pack as usize] += 1;
        p.inc();
    }
    p.stop();
    Ok(counts)
}

fn expire(ctx: &mut Context, common: &Common, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(&[msg::USAGE_EXPIRE], common.opts());
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (setup, target) = target(ctx, common)?;
    let config = repo::load_config(ctx, &setup)?;
    let Some(chain) = load_single(&target, msg::EXPIRE_INCREMENTAL)? else {
        return Ok(0);
    };
    let m = chain.tip();
    let progress = common.show_progress();
    let counts = referenced_counts(m, progress)?;
    let pack_dir = target.pack_dir();
    let mut dropped = BTreeSet::new();
    let mut p = rgit_progress::Progress::start(
        msg::PROGRESS_FINDING_UNREFERENCED,
        u64::from(m.num_packs()),
        progress,
    );
    for (i, name) in m.pack_names().iter().enumerate() {
        p.inc();
        if counts[i] != 0 {
            continue;
        }
        let idx = pack_dir.join(name);
        if idx.with_extension("keep").exists() || idx.with_extension("mtimes").exists() {
            continue;
        }
        if !idx.exists() {
            continue;
        }
        for f in mw::pack_files(&idx) {
            let _ = std::fs::remove_file(f);
        }
        dropped.insert(name.clone());
    }
    p.stop();
    if dropped.is_empty() {
        return Ok(0);
    }
    let opts = mw::WriteOptions {
        version: m.version(),
        progress,
        drop: dropped,
        ..Default::default()
    };
    let _ = &config;
    mw::write(&pack_dir, &target.pack_display(), target.kind, &opts).map_err(map_write_error)?;
    Ok(0)
}

fn repack(ctx: &mut Context, common: &Common, argv: &[String]) -> CmdResult {
    let batch = Cell::new(0u64);
    let mut opts = common.opts();
    opts.push(Opt::unsigned(None, "batch-size", &batch, msg::H_BATCH_SIZE));
    let mut parser = Parser::new(&[msg::USAGE_REPACK], opts);
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (setup, target) = target(ctx, common)?;
    let config = repo::load_config(ctx, &setup)?;
    let Some(chain) = load_single(&target, msg::REPACK_INCREMENTAL)? else {
        return Ok(0);
    };
    let m = chain.tip();
    let progress = common.show_progress();
    let pack_dir = target.pack_dir();
    let pack_kept = config
        .bool("repack.packkeptobjects")
        .ok()
        .flatten()
        .unwrap_or(false);
    let batch = batch.get();

    // Candidate packs: loadable, not cruft, not kept (unless packing kept
    // objects).
    struct Cand {
        id: u32,
        mtime: std::time::SystemTime,
        name: String,
        size: u64,
        objects: u64,
    }
    let mut cands: Vec<Cand> = Vec::new();
    let counts = {
        let mut c = vec![0u64; m.num_packs() as usize];
        for i in 0..m.len() {
            c[m.pack_id(i).map_err(Die::Fatal)? as usize] += 1;
        }
        c
    };
    for (i, name) in m.pack_names().iter().enumerate() {
        let idx = pack_dir.join(name);
        let pack = idx.with_extension("pack");
        let Ok(meta) = std::fs::metadata(&pack) else {
            continue;
        };
        if idx.with_extension("mtimes").exists() {
            continue;
        }
        if !pack_kept && idx.with_extension("keep").exists() {
            continue;
        }
        let Ok(index) = rgit_pack::PackIndex::open(&idx, target.kind) else {
            continue;
        };
        cands.push(Cand {
            id: i as u32,
            mtime: meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            name: name.clone(),
            size: meta.len(),
            objects: u64::from(index.len()),
        });
    }
    let selected: Vec<u32> = if batch == 0 {
        cands.iter().map(|c| c.id).collect()
    } else {
        cands.sort_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.name.cmp(&b.name)));
        let mut total = 0u64;
        let mut out = Vec::new();
        for c in &cands {
            if total >= batch {
                break;
            }
            let referenced = counts[c.id as usize];
            let expected = if c.objects == 0 {
                0
            } else {
                (u128::from(c.size) * u128::from(referenced) / u128::from(c.objects)) as u64
            };
            if expected >= batch {
                continue;
            }
            total += expected;
            out.push(c.id);
        }
        out
    };
    if selected.len() <= 1 {
        return Ok(0);
    }

    // The objects the MIDX takes from the selected packs.
    let chosen: HashSet<u32> = selected.into_iter().collect();
    let mut ids: Vec<ObjectId> = Vec::new();
    for i in 0..m.len() {
        if chosen.contains(&m.pack_id(i).map_err(Die::Fatal)?) {
            ids.push(m.id_at(i));
        }
    }
    let odb = repo::odb_with_config(&setup, &config)?;
    let described = rgit_pack::write::describe(&odb, target.kind, &ids)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let ordered = rgit_pack::write::pack_order(&described);
    let pack_opts = store::PackOptions::from_config(&config)?;
    store::write_pack(&odb, &pack_dir, &ordered, &pack_opts)?;

    let opts = mw::WriteOptions {
        version: m.version(),
        progress,
        ..Default::default()
    };
    mw::write(&pack_dir, &target.pack_display(), target.kind, &opts).map_err(map_write_error)?;
    Ok(0)
}

fn compact(ctx: &mut Context, common: &Common, argv: &[String]) -> CmdResult {
    let bitmap = Cell::new(0);
    let base: RefCell<Option<String>> = RefCell::new(None);
    let incremental = Cell::new(0);
    let chain_file = Cell::new(1);
    let mut opts = common.opts();
    opts.extend([
        Opt::string(None, "base", &base, "checksum", msg::H_BASE),
        Opt::bool(None, "bitmap", &bitmap, msg::H_BITMAP),
        Opt::bool(None, "incremental", &incremental, msg::H_INCREMENTAL),
        Opt::bool(None, "write-chain-file", &chain_file, msg::H_CHAIN_FILE),
    ]);
    let mut parser = Parser::new(&[msg::USAGE_COMPACT], opts);
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    if chain_file.get() == 0 && incremental.get() == 0 {
        return Err(parser
            .error_with_usage(msg::NO_CHAIN_WITHOUT_INCREMENTAL)
            .into());
    }
    drop(parser);
    let (setup, target) = target(ctx, common)?;
    let config = repo::load_config(ctx, &setup)?;
    if config.int("midx.version").ok().flatten() == Some(1) {
        return Err(Die::Fatal(msg::COMPACT_V1.into()));
    }
    let pack_dir = target.pack_dir();
    let mut errors = Vec::new();
    let chain = MidxChain::open(&pack_dir, target.kind, &mut errors);
    for e in &errors {
        eprintln!("error: {e}");
    }
    let chain = chain.map_err(|e| Die::Fatal(e.0))?;
    let layers = chain.map(MidxChain::into_layers).unwrap_or_default();
    let position = |name: &str| layers.iter().position(|l| l.checksum().to_hex() == name);
    let find = |name: &str| -> Result<usize, Die> {
        position(name).ok_or_else(|| Die::Fatal(msg::could_not_find_midx(name)))
    };
    let from = find(&args[0])?;
    let to = find(&args[1])?;
    if from == to {
        return Err(Die::Fatal(msg::COMPACT_UNIQUE.into()));
    }
    if from > to {
        return Err(Die::Fatal(msg::not_an_ancestor(&args[0], &args[1])));
    }
    // The layers below the result, and the first layer it replaces.
    let (below, start) = match base.take().as_deref() {
        None => (from, from),
        Some("none") => (0, 0),
        Some(b) => {
            let i = position(b)
                .filter(|&i| i < from)
                .ok_or_else(|| Die::Fatal(msg::could_not_find_base_midx(b)))?;
            (i + 1, i + 1)
        }
    };
    let bytes = midx::write::compact_bytes(target.kind, &layers[start..=to])
        .map_err(map_write_error)?;
    let sum = ObjectId::from_bytes(target.kind, &bytes[bytes.len() - target.kind.byte_len()..])
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let dir = pack_dir.join(midx::CHAIN_DIR);
    mw::write_file(&midx::layer_path(&pack_dir, &sum), &bytes).map_err(map_write_error)?;
    if chain_file.get() == 0 {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", sum.to_hex());
        return Ok(0);
    }
    let mut text = String::new();
    for l in layers[..below].iter() {
        text.push_str(&l.checksum().to_hex());
        text.push('\n');
    }
    text.push_str(&sum.to_hex());
    text.push('\n');
    for l in &layers[to + 1..] {
        text.push_str(&l.checksum().to_hex());
        text.push('\n');
    }
    mw::write_file(&dir.join(midx::CHAIN_FILE), text.as_bytes()).map_err(map_write_error)?;
    for l in &layers[start..=to] {
        if l.checksum() != sum {
            let _ = std::fs::remove_file(l.path());
        }
    }
    Ok(0)
}
