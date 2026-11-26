// SPDX-License-Identifier: MIT
//! `git fsck` (Documentation/git-fsck.adoc, fsck-msgids, config/fsck):
//! verify the objects of the repository and their connectivity.
//!
//! 1. The reference database is checked (`--references`, see `refs.rs`).
//! 2. Every object (loose objects, then packs; alternates too unless
//!    `--no-full`) is read and its hash verified, then checked with
//!    `rgit_object::fsck` at the severities of `fsck.<msg-id>`
//!    (`--strict` turns informational findings into errors). Its links are
//!    recorded. With `--connectivity-only` only links are read.
//! 3. Heads: the objects given on the command line, or all refs, `HEAD`
//!    of every worktree, the reflogs (unless `--no-reflogs`) and the
//!    indexes with their cache trees. Everything reachable from them is
//!    marked, with a name for `--name-objects`.
//! 4. All objects are reported in the order of git's object hash table:
//!    missing ones, broken links of reachable objects, and unreachable
//!    or dangling ones (`--lost-found` writes the dangling ones).
//!
//! The exit code combines 1 (bad objects), 2 (missing or unreachable
//! pointers), 4 (bad packs) and 8 (bad references) like git.

#[path = "fsck/gitmodules.rs"]
pub(crate) mod gitmodules;
#[path = "fsck/objects.rs"]
mod objects;
#[path = "fsck/refs.rs"]
mod refs;

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

use rgit_hash::{HashKind, ObjectId};
use rgit_object::ObjectKind;
use rgit_object::fsck::{MsgId, Severity};

use crate::messages::fsck as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;

pub(crate) const ERROR_OBJECT: i32 = 1;
pub(crate) const ERROR_REACHABLE: i32 = 2;
pub(crate) const ERROR_PACK: i32 = 4;
pub(crate) const ERROR_REFS: i32 = 8;

/// What the command line asked for.
#[derive(Clone, Debug, Default)]
pub(crate) struct Options {
    pub verbose: bool,
    pub unreachable: bool,
    pub dangling: bool,
    pub tags: bool,
    pub root: bool,
    pub cache: bool,
    pub reflogs: bool,
    pub full: bool,
    pub connectivity_only: bool,
    pub strict: bool,
    pub lost_found: bool,
    pub name_objects: bool,
    pub references: bool,
    /// Progress meters (`--progress`, else stderr is a terminal; never
    /// with `--verbose`).
    pub progress: bool,
}

/// A reference from one object to another.
#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub oid: ObjectId,
    /// The type the target must have.
    pub kind: ObjectKind,
    /// For tree entries: the entry name (with `/` for subtrees).
    pub path: Option<String>,
}

/// One object as fsck knows it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Obj {
    /// The type: of the object when it exists, else the type it is
    /// referred to as.
    pub kind: Option<ObjectKind>,
    /// A readable copy exists.
    pub has: bool,
    /// Another object refers to it.
    pub used: bool,
    /// Reachable from a head.
    pub reachable: bool,
    /// Its links (when it exists and could be parsed).
    pub links: Vec<Link>,
    /// How it is reachable (`--name-objects`).
    pub name: Option<String>,
    /// Insertion order, for collisions in the hash table emulation.
    pub seq: usize,
}

/// The state of one run.
pub(crate) struct Fsck {
    pub opts: Options,
    pub hash: HashKind,
    pub objs: HashMap<ObjectId, Obj>,
    pub errors: i32,
    pub severities: HashMap<MsgId, Severity>,
    pub skip: HashSet<ObjectId>,
    /// Objects in promisor packs and everything they refer to: missing
    /// ones are expected in a partial clone.
    pub promisor: HashSet<ObjectId>,
    /// `fsck.largePathname` limit.
    pub max_pathname_len: usize,
    /// `.gitattributes` / `.gitmodules` blobs named by trees.
    pub special: Vec<(ObjectId, rgit_object::fsck::SpecialFile)>,
    /// `core.bigFileThreshold`: larger blobs are checked as streams.
    pub big_file_threshold: u64,
}

impl Fsck {
    /// The entry of `oid`, created on first use.
    pub fn obj(&mut self, oid: &ObjectId) -> &mut Obj {
        let next = self.objs.len();
        self.objs.entry(*oid).or_insert_with(|| Obj {
            seq: next,
            ..Obj::default()
        })
    }

    /// The severity of a finding, after configuration and `--strict`.
    pub fn severity(&self, id: MsgId) -> Severity {
        let sev = self
            .severities
            .get(&id)
            .copied()
            .unwrap_or_else(|| id.default_severity());
        match sev {
            Severity::Fatal => Severity::Error,
            Severity::Warn if self.opts.strict => Severity::Error,
            Severity::Info => Severity::Warn,
            s => s,
        }
    }

    fn label(&self, oid: &ObjectId) -> String {
        match self.objs.get(oid).and_then(|o| o.name.as_ref()) {
            Some(n) if self.opts.name_objects => format!("{oid} ({n})"),
            _ => oid.to_string(),
        }
    }
}

fn kind_name(k: Option<ObjectKind>) -> &'static str {
    k.map_or("unknown", |k| k.name())
}

/// git's object table order: slots by the first four bytes of the id
/// (little-endian), the table twice as large as needed, linear probing.
fn table_size(n: usize) -> usize {
    let mut size = 0usize;
    for k in 0..n {
        if size == 0 || size - 1 <= 2 * k {
            size = if size < 32 { 32 } else { size * 2 };
        }
    }
    size
}

fn table_order(objs: &HashMap<ObjectId, Obj>) -> Vec<ObjectId> {
    fn slot(id: &ObjectId, mask: usize) -> usize {
        let b = id.as_bytes();
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize & mask
    }
    fn insert(slots: &mut [Option<ObjectId>], id: ObjectId) {
        let mask = slots.len() - 1;
        let mut i = slot(&id, mask);
        while slots[i].is_some() {
            i = (i + 1) & mask;
        }
        slots[i] = Some(id);
    }
    let mut by_seq: Vec<(&ObjectId, usize)> = objs.iter().map(|(id, o)| (id, o.seq)).collect();
    by_seq.sort_by_key(|&(_, s)| s);
    // Insert in the order the objects were met; growing the table
    // re-inserts the entries in slot order, as git does.
    let mut slots: Vec<Option<ObjectId>> = Vec::new();
    for (k, (id, _)) in by_seq.into_iter().enumerate() {
        if slots.is_empty() || slots.len() - 1 <= 2 * k {
            let size = if slots.len() < 32 { 32 } else { slots.len() * 2 };
            let old = std::mem::replace(&mut slots, vec![None; size]);
            for e in old.into_iter().flatten() {
                insert(&mut slots, e);
            }
        }
        insert(&mut slots, *id);
    }
    slots.into_iter().flatten().collect()
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let unreachable = Cell::new(false);
    let dangling = Cell::new(-1);
    let tags = Cell::new(false);
    let root = Cell::new(false);
    let cache = Cell::new(false);
    let reflogs = Cell::new(true);
    let full = Cell::new(true);
    let connectivity = Cell::new(false);
    let strict = Cell::new(false);
    let lost_found = Cell::new(false);
    let progress = Cell::new(-1);
    let name_objects = Cell::new(false);
    let references = Cell::new(true);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::H_VERBOSE),
            Opt::flag(None, "unreachable", &unreachable, msg::H_UNREACHABLE),
            Opt::bool(None, "dangling", &dangling, msg::H_DANGLING),
            Opt::flag(None, "tags", &tags, msg::H_TAGS),
            Opt::flag(None, "root", &root, msg::H_ROOT),
            Opt::flag(None, "cache", &cache, msg::H_CACHE),
            Opt::flag(None, "reflogs", &reflogs, msg::H_REFLOGS),
            Opt::flag(None, "full", &full, msg::H_FULL),
            Opt::flag(None, "connectivity-only", &connectivity, msg::H_CONNECTIVITY),
            Opt::flag(None, "strict", &strict, msg::H_STRICT),
            Opt::flag(None, "lost-found", &lost_found, msg::H_LOST_FOUND),
            Opt::bool(None, "progress", &progress, msg::H_PROGRESS),
            Opt::flag(None, "name-objects", &name_objects, msg::H_NAME_OBJECTS),
            Opt::flag(None, "references", &references, msg::H_REFERENCES),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let lost = lost_found.get();
    let opts = Options {
        verbose: verbose.get(),
        unreachable: unreachable.get(),
        // --lost-found implies dangling output.
        dangling: dangling.get() != 0 || lost,
        tags: tags.get(),
        root: root.get(),
        cache: cache.get(),
        reflogs: reflogs.get(),
        full: full.get(),
        connectivity_only: connectivity.get(),
        strict: strict.get(),
        lost_found: lost,
        name_objects: name_objects.get(),
        references: references.get(),
        progress: !verbose.get()
            && match progress.get() {
                -1 => rgit_progress::stderr_is_terminal(),
                v => v != 0,
            },
    };

    let setup = repo::setup(ctx, Mode::Required)?;
    let mut repo = Repo::open(ctx, setup)?;
    let cfg = fsck_config(&repo)?;
    let mut f = Fsck {
        opts: opts.clone(),
        hash: repo.hash,
        objs: HashMap::new(),
        errors: 0,
        severities: cfg.severities,
        skip: cfg.skip,
        max_pathname_len: cfg.max_pathname_len,
        promisor: HashSet::new(),
        special: Vec::new(),
        big_file_threshold: repo.odb.big_file_threshold(),
    };

    if opts.references {
        if opts.verbose {
            eprintln!("{}", msg::CHECKING_REF_DATABASE);
        }
        let meter = opts
            .progress
            .then(|| rgit_progress::Progress::start(msg::CHECKING_REF_DATABASE, 1, true));
        f.errors |= refs::verify(&repo, &f);
        if let Some(mut m) = meter {
            m.set(1);
            m.stop();
        }
    }

    objects::scan(&mut f, &repo);

    // Heads.
    let mut heads: Vec<(ObjectId, String)> = Vec::new();
    let mut default_refs = 0usize;
    if !args.is_empty() {
        for arg in &args {
            let resolved = crate::revision::Resolver::new(&mut repo).resolve(arg);
            match resolved {
                Ok(r) => {
                    if !f.objs.get(&r.oid).is_some_and(|o| o.has) {
                        eprintln!("error: {}", msg::invalid_sha1_pointer(arg, &r.oid.to_string()));
                        f.errors |= ERROR_REACHABLE;
                        continue;
                    }
                    default_refs += 1;
                    heads.push((r.oid, arg.clone()));
                }
                Err(_) => {
                    eprintln!("error: {}", msg::invalid_parameter(arg));
                    f.errors |= ERROR_OBJECT;
                }
            }
        }
        if opts.cache {
            refs::index_heads(&mut f, &repo, &mut heads);
        }
    } else {
        default_refs = refs::default_heads(&mut f, &repo, &mut heads);
        refs::index_heads(&mut f, &repo, &mut heads);
    }
    if default_refs == 0 {
        eprintln!("{}", msg::NO_DEFAULT_REFERENCES);
    }

    // Reachability: depth first, the last head first. Objects are marked
    // (and named) when they are first met; a missing object is reported
    // as a broken link of the object that met it first.
    let mut stack: Vec<ObjectId> = Vec::new();
    for (oid, name) in &heads {
        let o = f.obj(oid);
        if o.name.is_none() {
            o.name = Some(name.clone());
        }
        if !o.reachable {
            o.reachable = true;
            stack.push(*oid);
        }
    }
    // Parents of shallow commits are not there by design.
    let shallow: HashSet<ObjectId> = std::fs::read(repo.setup.common_dir.join("shallow"))
        .map(|d| {
            d.split(|&b| b == b'\n')
                .filter_map(|l| ObjectId::from_hex(repo.hash, l).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut broken = String::new();
    while let Some(oid) = stack.pop() {
        let o = &f.objs[&oid];
        if !o.has {
            continue;
        }
        let links = o.links.clone();
        let name = o.name.clone();
        let kind = o.kind;
        let mut parent_no = 0;
        let is_shallow = shallow.contains(&oid);
        for link in links {
            if is_shallow && kind == Some(ObjectKind::Commit) && link.kind == ObjectKind::Commit {
                continue;
            }
            let child = name
                .as_deref()
                .and_then(|n| child_name(n, kind, &link, &mut parent_no));
            let t = f.obj(&link.oid);
            if t.kind.is_none() {
                t.kind = Some(link.kind);
            }
            if t.name.is_none() {
                t.name = child;
            }
            if t.reachable {
                continue;
            }
            if !t.has && f.promisor.contains(&link.oid) {
                continue;
            }
            let t = f.obj(&link.oid);
            t.reachable = true;
            if t.has {
                stack.push(link.oid);
            } else {
                broken.push_str(&msg::broken_link(
                    kind_name(kind),
                    &f.label(&oid),
                    link.kind.name(),
                    &f.label(&link.oid),
                ));
                broken.push('\n');
                f.errors |= ERROR_REACHABLE;
            }
        }
    }
    print!("{broken}");

    report(&mut f, &repo)?;
    Ok(f.errors)
}

/// The name of a link target of an object named `name`.
fn child_name(name: &str, kind: Option<ObjectKind>, link: &Link, parent_no: &mut usize) -> Option<String> {
    match kind? {
        ObjectKind::Commit => match link.kind {
            ObjectKind::Tree => Some(format!("{name}:")),
            _ => {
                *parent_no += 1;
                if *parent_no == 1 {
                    Some(first_parent_name(name))
                } else {
                    Some(format!("{name}^{parent_no}"))
                }
            }
        },
        ObjectKind::Tag => Some(name.to_string()),
        ObjectKind::Tree => link.path.as_ref().map(|p| format!("{name}{p}")),
        ObjectKind::Blob => None,
    }
}

/// `X` -> `X^`, `X^` -> `X~2`, `X~n` -> `X~(n+1)`.
fn first_parent_name(name: &str) -> String {
    if let Some(i) = name.rfind('~') {
        if let Ok(n) = name[i + 1..].parse::<u64>() {
            return format!("{}~{}", &name[..i], n + 1);
        }
    }
    if let Some(base) = name.strip_suffix('^') {
        return format!("{base}~2");
    }
    format!("{name}^")
}

/// Print missing objects, broken links and unreachable objects in hash
/// table order.
fn report(f: &mut Fsck, repo: &Repo) -> Result<(), Die> {
    if f.opts.verbose {
        eprintln!("{}", msg::checking_connectivity(table_size(f.objs.len())));
    }
    let order = table_order(&f.objs);
    let mut out = std::io::stdout().lock();
    let mut lost: Vec<(ObjectId, ObjectKind)> = Vec::new();
    for oid in order {
        let o = &f.objs[&oid];
        if o.reachable {
            if !o.has {
                let _ = writeln!(out, "{}", msg::missing(kind_name(o.kind), &f.label(&oid)));
                f.errors |= ERROR_REACHABLE;
                continue;
            }
            continue;
        }
        if !o.has {
            continue;
        }
        let kind = o.kind.expect("existing objects have a type");
        if f.opts.unreachable {
            let _ = writeln!(out, "{}", msg::unreachable(kind.name(), &oid.to_string()));
        } else if !o.used && f.opts.dangling {
            let _ = writeln!(out, "{}", msg::dangling(kind.name(), &oid.to_string()));
        }
        if !o.used && f.opts.lost_found {
            lost.push((oid, kind));
        }
    }
    let _ = out.flush();
    if !lost.is_empty() {
        write_lost_found(repo, &lost)?;
    }
    Ok(())
}

/// `--lost-found`: dangling commits to `lost-found/commit/<id>`, other
/// objects to `lost-found/other/<id>` (blobs with their content, others
/// with their id).
fn write_lost_found(repo: &Repo, lost: &[(ObjectId, ObjectKind)]) -> Result<(), Die> {
    let base: PathBuf = repo.setup.common_dir.join("lost-found");
    for (oid, kind) in lost {
        let dir = base.join(if *kind == ObjectKind::Commit { "commit" } else { "other" });
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(oid.to_string());
        let content = if *kind == ObjectKind::Blob {
            match repo.odb.read(oid) {
                Ok(Some(o)) => o.data,
                _ => continue,
            }
        } else {
            format!("{oid}\n").into_bytes()
        };
        if std::fs::write(&path, content).is_err() {
            return Err(Die::Fatal(msg::could_not_create_lost_found(&path.display().to_string())));
        }
    }
    Ok(())
}

/// The parsed `fsck.*` configuration.
struct Config {
    severities: HashMap<MsgId, Severity>,
    skip: HashSet<ObjectId>,
    max_pathname_len: usize,
}

/// `fsck.<msg-id>` severities (validated like git does), `fsck.skipList`
/// and the limit of `fsck.largePathname=<severity>:<length>`.
fn fsck_config(repo: &Repo) -> Result<Config, Die> {
    let mut cfg = Config {
        severities: HashMap::new(),
        skip: HashSet::new(),
        max_pathname_len: rgit_object::fsck::Options::default().max_pathname_len,
    };
    for entry in repo.config.entries() {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        let Some(key) = name.strip_prefix("fsck.") else {
            continue;
        };
        if key.contains('.') {
            continue;
        }
        let value = match &entry.value {
            Some(v) => String::from_utf8_lossy(v).into_owned(),
            None => {
                eprintln!("error: {}", msg::missing_config_value(&name));
                return Err(Die::Fatal(msg::unable_to_parse_config(&name)));
            }
        };
        if key == "skiplist" {
            cfg.skip = read_skiplist(repo, &value)?;
            continue;
        }
        let Some(id) = MsgId::from_name(key) else {
            if crate::commands::mktag::is_known_id(key) {
                continue;
            }
            return Err(Die::Fatal(msg::unhandled_fsck_id(key)));
        };
        let (sev_name, limit) = match value.split_once(':') {
            Some((s, n)) if id == MsgId::LargePathname => (s, n.parse::<usize>().ok()),
            _ => (value.as_str(), None),
        };
        let sev = match sev_name.to_ascii_lowercase().as_str() {
            "error" => Severity::Error,
            "warn" => Severity::Warn,
            "ignore" => Severity::Ignore,
            _ => return Err(Die::Fatal(msg::unknown_severity(&value))),
        };
        if id.default_severity() == Severity::Fatal && sev != Severity::Error {
            return Err(Die::Fatal(msg::cannot_demote(key, &value)));
        }
        if let Some(n) = limit {
            cfg.max_pathname_len = n;
        }
        cfg.severities.insert(id, sev);
    }
    Ok(cfg)
}

fn read_skiplist(repo: &Repo, path: &str) -> Result<HashSet<ObjectId>, Die> {
    let data = std::fs::read(path).map_err(|_| Die::Fatal(msg::could_not_open_skiplist(path)))?;
    let mut skip = HashSet::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.split(|&b| b == b'#').next().unwrap_or(&[]).trim_ascii();
        if line.is_empty() {
            continue;
        }
        match ObjectId::from_hex(repo.hash, line) {
            Ok(oid) => {
                skip.insert(oid);
            }
            Err(_) => {
                return Err(Die::Fatal(msg::invalid_skiplist_line(&String::from_utf8_lossy(
                    line,
                ))));
            }
        }
    }
    Ok(skip)
}
