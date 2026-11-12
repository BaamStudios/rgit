// SPDX-License-Identifier: MIT
//! `git index-pack` (Documentation/git-index-pack.adoc): build the `.idx`
//! (and `.rev`) for a pack file, or store a pack read from stdin in
//! `objects/pack/`. The work is done by `rgit_pack::index_pack`; this file
//! handles the command line, the repository, the output files and the
//! `--strict` checks.
//!
//! Not supported yet: `--promisor` (#24), `--verify-stat` (verify-pack),
//! `fsck.<msg-id>` configuration (#95), `core.sharedRepository` file modes.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use rgit_config::{ConfigSet, LoadOptions, StaticRepoFacts, SystemEnv, sources};
use rgit_fs::TempFile;
use rgit_hash::{HashKind, ObjectId};
use rgit_object::fsck::{self, Control, MsgId, Severity};
use rgit_pack::index_pack::{
    self as ip, IndexVersion, IndexedPack, Input, ObjectDb, ObjectHook, Options, Phase, Progress,
    messages as m,
};
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_pack::ObjectKind;

use crate::prelude::*;

/// What the command line asked for.
#[derive(Default)]
struct Args {
    verbose: bool,
    stdin: bool,
    fix_thin: bool,
    /// `--strict`: fsck plus connectivity.
    strict: bool,
    /// `--fsck-objects` or `--strict`.
    fsck: bool,
    /// `<msg-id>=<severity>` overrides.
    fsck_msgs: Vec<(String, String)>,
    self_contained: bool,
    verify: bool,
    keep: Option<String>,
    threads: Option<usize>,
    pack_header: Option<(u32, u32)>,
    index_version: Option<IndexVersion>,
    output: Option<String>,
    max_input_size: Option<u64>,
    object_format: Option<HashKind>,
    rev_index: Option<bool>,
    progress_title: Option<String>,
    pack: Option<String>,
}

fn usage() -> Die {
    Die::Output(rgit_options::Exit {
        code: rgit_options::EXIT_USAGE,
        stdout: String::new(),
        stderr: format!("usage: {}\n", m::USAGE),
    })
}

/// `strtoul`-like: decimal, `0x` hex or leading-zero octal.
fn parse_c_ulong(s: &str) -> Option<u64> {
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if s.len() > 1 && s.starts_with('0') {
        u64::from_str_radix(&s[1..], 8).ok()
    } else {
        s.parse().ok()
    }
}

/// A size with an optional `k`, `m` or `g` suffix.
fn parse_size(s: &str) -> Option<u64> {
    let (num, mult) = match s.char_indices().last() {
        Some((i, 'k' | 'K')) => (&s[..i], 1u64 << 10),
        Some((i, 'm' | 'M')) => (&s[..i], 1 << 20),
        Some((i, 'g' | 'G')) => (&s[..i], 1 << 30),
        _ => (s, 1),
    };
    num.parse::<u64>().ok()?.checked_mul(mult)
}

fn parse_index_version(arg: &str, value: &str) -> Result<IndexVersion, Die> {
    let bad = || Die::Fatal(m::bad_arg(arg));
    let (v, off) = match value.split_once(',') {
        Some((v, off)) => (v, Some(off)),
        None => (value, None),
    };
    let version: u32 = v.parse().map_err(|_| bad())?;
    if version != 1 && version != 2 {
        return Err(bad());
    }
    let mut out = IndexVersion {
        version,
        ..IndexVersion::default()
    };
    if let Some(off) = off.filter(|o| !o.is_empty()) {
        out.large_offset_limit = parse_c_ulong(off).ok_or_else(bad)?;
    }
    Ok(out)
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let mut a = Args::default();
    if argv.len() == 2 && (argv[1] == "-h" || argv[1] == "--help-all") {
        return Err(Die::Output(rgit_options::Exit {
            code: 0,
            stdout: format!("usage: {}\n", m::USAGE),
            stderr: String::new(),
        }));
    }
    let mut i = 1;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;
        if !arg.starts_with('-') {
            if a.pack.is_some() {
                return Err(usage());
            }
            a.pack = Some(arg.to_string());
            continue;
        }
        let (name, value) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (arg, None),
        };
        match (name, value) {
            ("--stdin", None) => a.stdin = true,
            ("--fix-thin", None) => a.fix_thin = true,
            ("--strict", v) | ("--fsck-objects", v) => {
                if name == "--strict" {
                    a.strict = true;
                }
                a.fsck = true;
                if let Some(v) = v {
                    for item in v.split([',', ' ']).filter(|s| !s.is_empty()) {
                        if let Some((id, sev)) = item.split_once(['=', ':']) {
                            a.fsck_msgs.push((id.to_string(), sev.to_string()));
                        }
                    }
                }
            }
            ("--check-self-contained-and-connected", None) => a.self_contained = true,
            ("--verify", None) => a.verify = true,
            ("--keep", v) => a.keep = Some(v.unwrap_or("").to_string()),
            ("--threads", Some(v)) => {
                a.threads = Some(v.parse::<usize>().map_err(|_| usage())?);
            }
            ("--pack_header", Some(v)) => {
                let (ver, n) = v.split_once(',').ok_or_else(usage)?;
                let ver = ver.parse().map_err(|_| usage())?;
                let n = n.parse().map_err(|_| usage())?;
                a.pack_header = Some((ver, n));
            }
            ("--index-version", Some(v)) => a.index_version = Some(parse_index_version(arg, v)?),
            ("--max-input-size", Some(v)) => {
                a.max_input_size = Some(parse_size(v).ok_or_else(usage)?);
            }
            ("--object-format", Some(v)) => {
                a.object_format =
                    Some(HashKind::from_name(v).ok_or_else(|| Die::Fatal(m::unknown_hash(v)))?);
            }
            ("--rev-index", None) => a.rev_index = Some(true),
            ("--no-rev-index", None) => a.rev_index = Some(false),
            ("--progress-title", None) => {
                let t = argv.get(i).ok_or_else(usage)?;
                i += 1;
                a.progress_title = Some(t.clone());
            }
            ("-o", None) => {
                let o = argv.get(i).ok_or_else(usage)?;
                i += 1;
                a.output = Some(o.clone());
            }
            ("-v", None) => a.verbose = true,
            // Accepted for compatibility; rgit does not print these.
            ("--show-resolving-progress", None) | ("--report-end-of-input", None) => {}
            _ => return Err(usage()),
        }
    }
    if a.pack.is_none() && !a.stdin {
        return Err(usage());
    }
    Ok(a)
}

/// The repository, if there is one.
struct Repo {
    git_dir: PathBuf,
    common_dir: PathBuf,
    worktree: Option<PathBuf>,
    hash: HashKind,
}

impl Repo {
    fn objects_dir(&self) -> PathBuf {
        match std::env::var_os("GIT_OBJECT_DIRECTORY").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => self.common_dir.join("objects"),
        }
    }
}

/// Gentle setup: no repository is fine.
fn discover(ctx: &Context) -> Result<Option<Repo>, Die> {
    let env = SystemEnv::default();
    let mut opts = rgit_repo::SetupOptions::from_process().map_err(crate::repo::die)?;
    if let Some(d) = &ctx.git_dir {
        opts.git_dir = Some(d.into());
    }
    if let Some(w) = &ctx.work_tree {
        opts.work_tree = Some(w.into());
    }
    if ctx.bare {
        opts.implicit_work_tree = false;
    }
    opts.support.sha256 = true;
    let mut protected =
        rgit_repo::StandardProtectedConfig::new(&env, crate::repo::command_line(ctx));
    match rgit_repo::discover(&opts, &mut protected) {
        Err(e) => Err(crate::repo::die(e)),
        Ok(Err(_)) => Ok(None),
        Ok(Ok(setup)) => {
            for w in &setup.warnings {
                eprintln!("warning: {w}");
            }
            // Paths on the command line stay relative to the directory the
            // command was started in.
            let abs = |p: &Path| std::path::absolute(setup.cwd.join(p)).unwrap_or(p.to_path_buf());
            Ok(Some(Repo {
                git_dir: abs(&setup.git_dir),
                common_dir: abs(&setup.common_dir),
                worktree: setup.work_tree.clone(),
                hash: setup.format.object_format,
            }))
        }
    }
}

fn load_config(ctx: &Context, repo: Option<&Repo>) -> ConfigSet {
    let env = SystemEnv::default();
    let facts = repo.map(|r| StaticRepoFacts {
        git_dir: Some(r.git_dir.clone()),
        worktree: r.worktree.clone(),
        branch: None,
    });
    let opts = LoadOptions {
        env: &env,
        repo: facts.as_ref().map(|f| f as &dyn rgit_config::RepoFacts),
        includes: true,
    };
    let repo_cfg = repo.map(|r| sources::RepoConfig {
        common_dir: r.common_dir.clone(),
        git_dir: r.git_dir.clone(),
        worktree_config: None,
    });
    let mut set = ConfigSet::new();
    let _ = sources::load_standard(
        opts,
        repo_cfg.as_ref(),
        &crate::repo::command_line(ctx),
        sources::ScopeFilter::default(),
        &mut set,
    );
    set
}

/// `--strict` / `--fsck-objects` / `--check-self-contained-and-connected`.
struct Checks {
    hash: HashKind,
    fsck: bool,
    severities: HashMap<MsgId, Severity>,
    /// Collected links, when connectivity is checked.
    links: Option<Mutex<Vec<ObjectId>>>,
    /// `.gitmodules` / `.gitattributes` blobs named by trees, to check
    /// after indexing (with `fsck`).
    queued: Mutex<Vec<(ObjectId, fsck::SpecialFile)>>,
}

impl Checks {
    fn severity(&self, id: MsgId) -> Severity {
        self.severities
            .get(&id)
            .copied()
            .unwrap_or_else(|| id.default_severity())
    }

    fn collect_links(&self, kind: ObjectKind, data: &[u8], out: &Mutex<Vec<ObjectId>>) {
        let mut found = Vec::new();
        match kind {
            ObjectKind::Tree => {
                for e in rgit_object::TreeIter::new(self.hash, data) {
                    let Ok(e) = e else { break };
                    // Submodule commits live in another repository.
                    if e.mode_bits & 0o170000 != 0o160000 {
                        found.push(e.oid);
                    }
                }
            }
            ObjectKind::Commit => {
                if let Ok(c) = rgit_object::Commit::parse(self.hash, data) {
                    found.extend(c.tree());
                    if let Ok(p) = c.parents() {
                        found.extend(p);
                    }
                }
            }
            ObjectKind::Tag => {
                if let Ok(t) = rgit_object::Tag::parse(self.hash, data) {
                    found.extend(t.object());
                }
            }
            ObjectKind::Blob => {}
        }
        if !found.is_empty() {
            out.lock().unwrap_or_else(|p| p.into_inner()).extend(found);
        }
    }
}

impl ObjectHook for Checks {
    fn object(&self, id: &ObjectId, kind: ObjectKind, data: &[u8]) -> Result<(), String> {
        if let Some(links) = &self.links {
            self.collect_links(kind, data, links);
        }
        if !self.fsck {
            return Ok(());
        }
        if kind == ObjectKind::Tree {
            let mut found = Vec::new();
            for e in rgit_object::TreeIter::new(self.hash, data) {
                let Ok(e) = e else { break };
                if e.mode_bits & 0o170000 != 0o100000 {
                    continue;
                }
                if let Some(f @ (fsck::SpecialFile::Gitmodules | fsck::SpecialFile::Gitattributes)) =
                    fsck::special_file(e.name)
                {
                    found.push((e.oid, f));
                }
            }
            if !found.is_empty() {
                self.queued
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend(found);
            }
        }
        let okind = match kind {
            ObjectKind::Commit => rgit_object::ObjectKind::Commit,
            ObjectKind::Tree => rgit_object::ObjectKind::Tree,
            ObjectKind::Blob => rgit_object::ObjectKind::Blob,
            ObjectKind::Tag => rgit_object::ObjectKind::Tag,
        };
        let mut failed = false;
        let mut lines = Vec::new();
        fsck::check_object(
            okind,
            self.hash,
            data,
            &fsck::Options::default(),
            &mut |f| {
                match self.severity(f.id) {
                    Severity::Ignore => {}
                    Severity::Warn | Severity::Info => {
                        lines.push(format!("warning: {}", m::fsck_finding(id, &f)));
                    }
                    Severity::Error | Severity::Fatal => {
                        if let Some(c) = &f.cause {
                            lines.push(format!("error: {c}"));
                        }
                        lines.push(format!("error: {}", m::fsck_finding(id, &f)));
                        failed = true;
                        return Control::Stop;
                    }
                }
                Control::Continue
            },
        );
        if !lines.is_empty() {
            let mut err = io::stderr().lock();
            for l in lines {
                let _ = writeln!(err, "{l}");
            }
        }
        if failed {
            Err(m::FSCK_ERROR.to_string())
        } else {
            Ok(())
        }
    }
}

/// git-style progress on stderr.
struct Meter {
    title: String,
    reading_title: String,
    total: u64,
    done: u64,
    bytes: u64,
    last_percent: Option<u64>,
    start: Instant,
    throughput: bool,
    phase: Phase,
}

impl Meter {
    fn line(&self) -> String {
        let pct = (self.done * 100).checked_div(self.total).unwrap_or(100);
        format!("{}: {pct:3}% ({}/{})", self.title, self.done, self.total)
    }
}

impl Progress for Meter {
    fn start(&mut self, phase: Phase, total: u64) {
        self.phase = phase;
        self.title = match phase {
            Phase::Reading => self.reading_title.clone(),
            Phase::Resolving => m::PROGRESS_RESOLVING.to_string(),
        };
        self.total = total;
        self.done = 0;
        self.bytes = 0;
        self.last_percent = None;
        self.start = Instant::now();
    }

    fn update(&mut self, done: u64, bytes: u64) {
        self.done = done;
        if self.phase == Phase::Reading {
            self.bytes = bytes;
        }
        let pct = (done * 100).checked_div(self.total).unwrap_or(100);
        if self.last_percent != Some(pct) {
            self.last_percent = Some(pct);
            let _ = write!(io::stderr(), "{}\r", self.line());
        }
    }

    fn finish(&mut self, suffix: Option<String>) {
        let mut line = self.line();
        if self.phase == Phase::Reading && self.throughput && self.bytes >= 1 << 20 {
            let secs = self.start.elapsed().as_secs_f64().max(1e-3);
            line.push_str(&format!(
                ", {} | {}",
                m::humanise_bytes(self.bytes),
                m::humanise_rate((self.bytes as f64 / secs) as u64)
            ));
        }
        let end = suffix.unwrap_or_else(|| "done".to_string());
        let _ = writeln!(io::stderr(), "{line}, {end}.");
    }
}

/// Removes a file on drop unless disarmed.
struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn fail(e: ip::Error) -> Die {
    for line in e.errors_before() {
        eprintln!("error: {line}");
    }
    Die::Fatal(e.to_string())
}

/// `x.pack` -> `x.<ext>`; `None` if `name` does not end in `.pack`.
fn sibling(name: &str, ext: &str) -> Option<String> {
    name.strip_suffix(".pack").map(|b| format!("{b}.{ext}"))
}

/// The `.rev` name for an index name.
fn rev_name(idx: &str) -> String {
    match idx.strip_suffix(".idx") {
        Some(b) => format!("{b}.rev"),
        None => format!("{idx}.rev"),
    }
}

/// Write `data` to `dest` through a temporary file in its directory.
fn write_file(dest: &Path, prefix: &str, data: &[u8]) -> Result<(), Die> {
    let dir = match dest.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let mut tmp = TempFile::with_mode(&dir, prefix, 0o444)
        .map_err(|e| Die::Fatal(m::cannot_create(&dir.join(format!("{prefix}XXXXXX")), &e)))?;
    tmp.write_all(data)
        .map_err(|e| Die::Fatal(m::cannot_create(tmp.path(), &e)))?;
    tmp.persist(dest)
        .map_err(|e| Die::Fatal(m::cannot_rename(dest, &e)))
}

/// Create the `.keep` file; `Ok(false)` if it already exists.
fn write_keep(path: &Path, msg: &str) -> Result<bool, Die> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => {
            if !msg.is_empty() {
                f.write_all(format!("{msg}\n").as_bytes())
                    .map_err(|e| Die::Fatal(m::cannot_write_keep(path, &e)))?;
            }
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(Die::Fatal(m::cannot_write_keep(path, &e))),
    }
}

fn config_bool(cfg: &ConfigSet, key: &str) -> Option<bool> {
    cfg.bool(key).ok().flatten()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = parse_args(argv)?;
    let repo = discover(ctx)?;
    let cfg = load_config(ctx, repo.as_ref());

    if args.fix_thin && !args.stdin {
        return Err(Die::Fatal(m::FIX_THIN_NEEDS_STDIN.into()));
    }
    if args.stdin && args.object_format.is_some() {
        return Err(Die::Fatal(m::OBJECT_FORMAT_WITH_STDIN.into()));
    }
    if args.stdin && repo.is_none() {
        return Err(Die::Fatal(m::STDIN_NEEDS_REPO.into()));
    }
    if args.verify && args.pack.is_none() {
        return Err(Die::Fatal(m::VERIFY_NEEDS_NAME.into()));
    }
    let hash = args
        .object_format
        .or(repo.as_ref().map(|r| r.hash))
        .unwrap_or(HashKind::Sha1);

    // Output names.
    let index_name: Option<String> = match (&args.output, &args.pack) {
        (Some(o), _) => Some(o.clone()),
        (None, Some(p)) => Some(sibling(p, "idx").ok_or_else(|| Die::Fatal(m::not_a_pack_name(p)))?),
        (None, None) => None,
    };
    let keep_name: Option<String> = match &args.pack {
        Some(p) => sibling(p, "keep").or_else(|| {
            let i = index_name.as_deref()?;
            Some(format!("{}.keep", i.strip_suffix(".idx").unwrap_or(i)))
        }),
        None => None,
    };

    let mut version = args.index_version.unwrap_or_else(|| {
        let v = cfg.int("pack.indexversion").ok().flatten().unwrap_or(2);
        IndexVersion {
            version: if v == 1 { 1 } else { 2 },
            ..IndexVersion::default()
        }
    });
    let write_rev = args.rev_index.unwrap_or_else(|| {
        match std::env::var("GIT_TEST_WRITE_REV_INDEX") {
            Ok(v) if !v.is_empty() => rgit_config::value::parse_bool(Some(v.as_bytes())).unwrap_or(true),
            _ => config_bool(&cfg, "pack.writereverseindex").unwrap_or(true),
        }
    });
    let threads = args.threads.unwrap_or_else(|| {
        cfg.int("pack.threads")
            .ok()
            .flatten()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0)
    });
    // Objects appended by --fix-thin are compressed like loose objects.
    let compression = cfg
        .int("core.loosecompression")
        .ok()
        .flatten()
        .or_else(|| cfg.int("core.compression").ok().flatten())
        .unwrap_or(1) as i32;

    // --verify: the index to compare with.
    let existing_index = if args.verify {
        // The pack next to the index must exist, and the index must be
        // readable.
        let name = index_name.as_deref().expect("pack name checked");
        let pack_of_index = format!("{}.pack", name.strip_suffix(".idx").unwrap_or(name));
        if !Path::new(&pack_of_index).is_file() {
            return Err(Die::Fatal(m::cannot_open_pack(name)));
        }
        let cannot = || Die::Fatal(m::cannot_open_index(Path::new(name)));
        let data = std::fs::read(name).map_err(|_| cannot())?;
        if let Err(e) = rgit_pack::PackIndex::from_bytes(data.clone(), name, hash) {
            eprintln!("error: {e}");
            return Err(cannot());
        }
        if args.index_version.is_none() {
            if let Some(v) = IndexVersion::of(&data, hash) {
                version = v;
            }
        }
        Some(data)
    } else {
        None
    };

    // Packs, loose objects and alternates of the repository.
    let odb = repo
        .as_ref()
        .map(|r| ObjectDatabase::open_with_packs(r.objects_dir(), hash, LooseOptions::default()));
    let checks = Checks {
        hash,
        fsck: args.fsck,
        severities: args
            .fsck_msgs
            .iter()
            .filter_map(|(id, sev)| {
                let id = MsgId::from_name(id)?;
                let sev = match sev.to_ascii_lowercase().as_str() {
                    "error" => Severity::Error,
                    "warn" => Severity::Warn,
                    "ignore" => Severity::Ignore,
                    _ => return None,
                };
                Some((id, sev))
            })
            .collect(),
        links: (args.strict || args.self_contained).then(|| Mutex::new(Vec::new())),
        queued: Mutex::new(Vec::new()),
    };
    let hook: Option<&dyn ObjectHook> =
        (args.fsck || args.self_contained).then_some(&checks as &dyn ObjectHook);

    let mut opts = Options::new(hash);
    opts.threads = threads;
    opts.max_input_size = args.max_input_size;
    opts.fix_thin = args.fix_thin;
    opts.reject_duplicates = args.strict || args.verify;
    opts.pack_header = args.pack_header;
    opts.compression_level = compression;
    // Data after the pack on a pipe belongs to the caller and is passed
    // on to stdout; in a regular file it is junk.
    opts.allow_trailing_data = args.stdin && !stdin_is_file();

    let mut meter = Meter {
        title: String::new(),
        reading_title: args.progress_title.clone().unwrap_or_else(|| {
            if args.stdin {
                m::PROGRESS_RECEIVING.to_string()
            } else {
                m::PROGRESS_INDEXING.to_string()
            }
        }),
        total: 0,
        done: 0,
        bytes: 0,
        last_percent: None,
        start: Instant::now(),
        throughput: args.stdin,
        phase: Phase::Reading,
    };
    let progress: Option<&mut (dyn Progress + 'static)> =
        args.verbose.then_some(&mut meter as &mut (dyn Progress + 'static));
    let db: Option<&dyn ObjectDb> = odb.as_ref().map(|o| o as &dyn ObjectDb);

    // Read the pack.
    let mut pack_tmp: Option<TempFile> = None;
    let mut pack_guard = RemoveOnDrop(None);
    let indexed: IndexedPack = if args.stdin {
        let (mut file, path) = match &args.pack {
            Some(p) => {
                let mut o = OpenOptions::new();
                o.read(true).write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    o.mode(0o444);
                }
                let f = o
                    .open(p)
                    .map_err(|e| Die::Fatal(m::cannot_create(Path::new(p), &e)))?;
                pack_guard.0 = Some(PathBuf::from(p));
                (f, PathBuf::from(p))
            }
            None => {
                let dir = repo.as_ref().expect("checked").objects_dir().join("pack");
                let _ = std::fs::create_dir_all(&dir);
                let mut t = TempFile::with_mode(&dir, rgit_fs::PACK_PREFIX, 0o444).map_err(|e| {
                    Die::Fatal(m::cannot_create(&dir.join("tmp_pack_XXXXXX"), &e))
                })?;
                let f = t
                    .file_mut()
                    .try_clone()
                    .map_err(|e| Die::Fatal(m::cannot_create(t.path(), &e)))?;
                let p = t.path().to_path_buf();
                pack_tmp = Some(t);
                (f, p)
            }
        };
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        ip::index_pack(
            Input::Stream {
                reader: &mut lock,
                dest: &mut file,
                dest_path: &path,
            },
            &opts,
            db,
            hook,
            progress,
        )
        .map_err(fail)?
    } else {
        let p = args.pack.as_deref().expect("checked");
        ip::index_pack(Input::File(Path::new(p)), &opts, db, hook, progress).map_err(fail)?
    };

    // Special blobs named by trees but not in the pack: they must be
    // looked up locally. A missing `.gitmodules` blob is reported to the
    // caller after the pack name.
    let mut missing_gitmodules = Vec::new();
    {
        let queued = std::mem::take(&mut *checks.queued.lock().unwrap_or_else(|p| p.into_inner()));
        let in_pack: HashSet<ObjectId> = indexed.objects().iter().map(|o| o.id).collect();
        for (id, f) in queued {
            if in_pack.contains(&id) {
                continue;
            }
            let Some(db) = &odb else {
                return Err(Die::Fatal(m::QUEUED_OUTSIDE_REPO.into()));
            };
            if f == fsck::SpecialFile::Gitmodules && !db.contains(&id) && !missing_gitmodules.contains(&id) {
                missing_gitmodules.push(id);
            }
        }
    }

    // Connectivity.
    if let Some(links) = &checks.links {
        let links = std::mem::take(&mut *links.lock().unwrap_or_else(|p| p.into_inner()));
        let have: HashSet<ObjectId> = indexed.objects().iter().map(|o| o.id).collect();
        for l in links {
            if have.contains(&l) {
                continue;
            }
            let local = !args.self_contained && odb.as_ref().is_some_and(|o| o.contains(&l));
            if !local {
                return Err(Die::Fatal(m::missing_link(&l)));
            }
        }
    }

    let index = indexed.index_bytes(version).map_err(fail)?;
    if let Some(old) = existing_index {
        let name = index_name.expect("checked");
        if old != index {
            return Err(Die::Fatal(m::index_differs(Path::new(&name))));
        }
        if write_rev {
            let rev = rev_name(&name);
            if let Ok(old_rev) = std::fs::read(&rev) {
                if old_rev != indexed.rev_bytes().map_err(fail)? {
                    return Err(Die::Fatal(m::index_differs(Path::new(&rev))));
                }
            }
        }
        pack_guard.0 = None;
        return Ok(0);
    }

    let hex = indexed.checksum().to_hex();
    let rev = if write_rev {
        Some(indexed.rev_bytes().map_err(fail)?)
    } else {
        None
    };
    let mut kept = false;
    match pack_tmp {
        Some(tmp) => {
            let dir = tmp.path().parent().expect("in objects/pack").to_path_buf();
            let base = dir.join(format!("pack-{hex}"));
            let with = |ext: &str| base.with_extension(ext);
            if let Some(msg) = &args.keep {
                kept = write_keep(&with("keep"), msg)?;
            }
            tmp.persist(with("pack"))
                .map_err(|e| Die::Fatal(m::cannot_rename(&with("pack"), &e)))?;
            let idx = match &index_name {
                Some(i) => PathBuf::from(i),
                None => with("idx"),
            };
            if let Some(rev) = &rev {
                let rev_path = match &index_name {
                    Some(i) => PathBuf::from(rev_name(i)),
                    None => with("rev"),
                };
                write_file(&rev_path, "tmp_rev_", rev)?;
            }
            write_file(&idx, "tmp_idx_", &index)?;
        }
        None => {
            let idx = index_name.expect("named pack");
            if let (Some(msg), Some(k)) = (&args.keep, &keep_name) {
                kept = write_keep(Path::new(k), msg)?;
            }
            if let Some(rev) = &rev {
                write_file(Path::new(&rev_name(&idx)), "tmp_rev_", rev)?;
            }
            write_file(Path::new(&idx), "tmp_idx_", &index)?;
        }
    }
    pack_guard.0 = None;

    let mut out = io::stdout().lock();
    let line = if args.stdin {
        format!("{}\t{hex}\n", if kept { "keep" } else { "pack" })
    } else {
        format!("{hex}\n")
    };
    let _ = out.write_all(line.as_bytes());
    for id in &missing_gitmodules {
        let _ = writeln!(out, "{id}");
    }
    let _ = out.write_all(indexed.trailing_data());
    let _ = out.flush();
    Ok(0)
}

/// Whether standard input is a regular file (not a pipe or terminal).
fn stdin_is_file() -> bool {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map(std::fs::File::from)
            .and_then(|f| f.metadata())
            .is_ok_and(|m| m.is_file())
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_version_argument() {
        let v = parse_index_version("--index-version=2,0x40000", "2,0x40000").ok().unwrap();
        assert_eq!(v.version, 2);
        assert_eq!(v.large_offset_limit, 0x40000);
        let v = parse_index_version("--index-version=1", "1").ok().unwrap();
        assert_eq!(v.version, 1);
        assert!(parse_index_version("--index-version=3", "3").is_err());
        assert!(parse_index_version("--index-version=x", "x").is_err());
        assert!(parse_index_version("--index-version=2,", "2,").is_ok());
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("20"), Some(20));
        assert_eq!(parse_size("2k"), Some(2048));
        assert_eq!(parse_size("1m"), Some(1 << 20));
        assert_eq!(parse_size("x"), None);
    }
}
