// SPDX-License-Identifier: MIT
//! The repository behind the revision parser (`rgit_revwalk::revision`):
//! adapters from refs, objects, index, configuration and work tree to its
//! lookup traits, plus the settings it reads from the configuration.
//!
//! The object lookup reads loose objects and packs directly; it is a
//! stopgap until the object database crate (#14, PR #207) lands, which will
//! replace [`Objects`]. Reflogs are read from `logs/` the same way until
//! #30 provides them.

use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;

use rgit_config::{ConfigSet, Origin};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_index::{Index, ReadOptions, Stage};
use rgit_object::ObjectKind;
use rgit_pack::{DeltaBaseCache, PackSet};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;
use rgit_revwalk::revision::{
    ConfigLookup, Hint, IndexLookup, LookupError, ObjectLookup, Options, PushDefault, RefLookup,
    RefValue, ReflogEntry, WorkTreeInfo, WorkTreeLookup,
};

use crate::Die;

/// Loose objects and packs of one object directory.
pub struct Objects {
    dir: PathBuf,
    kind: HashKind,
    packs: PackSet,
    cache: RefCell<DeltaBaseCache>,
    count: OnceCell<u64>,
    shallow: HashSet<ObjectId>,
}

impl Objects {
    /// The objects in `dir`; `shallow` is the repository's `shallow` file.
    pub fn new(dir: PathBuf, kind: HashKind, shallow: &std::path::Path) -> Objects {
        let packs = PackSet::open(dir.join("pack"), kind);
        let shallow = std::fs::read_to_string(shallow)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| ObjectId::from_hex(kind, l.trim().as_bytes()).ok())
            .collect();
        Objects {
            dir,
            kind,
            packs,
            cache: RefCell::new(DeltaBaseCache::new(
                rgit_pack::DEFAULT_DELTA_BASE_CACHE_LIMIT,
            )),
            count: OnceCell::new(),
            shallow,
        }
    }

    fn loose_path(&self, oid: &ObjectId) -> PathBuf {
        let hex = oid.to_hex();
        self.dir.join(&hex[..2]).join(&hex[2..])
    }

    fn read_loose(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError> {
        let path = self.loose_path(oid);
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => return Ok(None),
        };
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(file)
            .read_to_end(&mut raw)
            .map_err(|_| LookupError(format!("unable to unpack {} header", oid.to_hex())))?;
        let nul = raw
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| LookupError(format!("unable to unpack {} header", oid.to_hex())))?;
        let header = &raw[..nul];
        let sp = header
            .iter()
            .position(|&b| b == b' ')
            .unwrap_or(header.len());
        let kind = ObjectKind::from_name(&header[..sp])
            .ok_or_else(|| LookupError("invalid object type".to_string()))?;
        Ok(Some((kind, raw[nul + 1..].to_vec())))
    }

    fn loose_prefix(&self, prefix: &Prefix) -> Vec<ObjectId> {
        let hex = prefix.as_id().to_hex_with_len(prefix.hex_len());
        let mut out = Vec::new();
        let Ok(dir) = std::fs::read_dir(self.dir.join(&hex[..2])) else {
            return out;
        };
        for e in dir.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with(&hex[2..]) {
                continue;
            }
            let full = format!("{}{name}", &hex[..2]);
            if let Ok(oid) = ObjectId::from_hex(self.kind, full.as_bytes()) {
                out.push(oid);
            }
        }
        out
    }
}

fn pack_kind(k: rgit_pack::ObjectKind) -> ObjectKind {
    match k {
        rgit_pack::ObjectKind::Commit => ObjectKind::Commit,
        rgit_pack::ObjectKind::Tree => ObjectKind::Tree,
        rgit_pack::ObjectKind::Blob => ObjectKind::Blob,
        rgit_pack::ObjectKind::Tag => ObjectKind::Tag,
    }
}

impl ObjectLookup for Objects {
    fn hash_kind(&self) -> HashKind {
        self.kind
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError> {
        if let Some(o) = self.read_loose(oid)? {
            return Ok(Some(o));
        }
        let mut cache = self.cache.borrow_mut();
        match self.packs.read(oid, &mut cache, &mut |_| {}) {
            Ok(Some(o)) => Ok(Some((pack_kind(o.kind), o.data))),
            Ok(None) => Ok(None),
            Err(e) => Err(LookupError(e.to_string())),
        }
    }

    fn prefix_matches(&self, prefix: &Prefix) -> Result<Vec<ObjectId>, LookupError> {
        let mut out = self.loose_prefix(prefix);
        out.extend(self.packs.lookup_prefix(prefix, usize::MAX));
        Ok(out)
    }

    fn is_shallow(&self, oid: &ObjectId) -> bool {
        self.shallow.contains(oid)
    }

    fn approximate_count(&self) -> u64 {
        *self.count.get_or_init(|| {
            let mut n: u64 = self
                .packs
                .packs()
                .iter()
                .map(|p| u64::from(p.index().len()))
                .sum();
            for i in 0..256 {
                if let Ok(d) = std::fs::read_dir(self.dir.join(format!("{i:02x}"))) {
                    n += d.count() as u64;
                }
            }
            n
        })
    }
}

/// References from the files backend; reflogs from `logs/`.
pub struct Refs {
    store: FilesStore,
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: HashKind,
}

impl Refs {
    pub fn new(setup: &Setup, kind: HashKind) -> Refs {
        Refs {
            store: FilesStore::new(&setup.git_dir, kind),
            git_dir: setup.git_dir.clone(),
            common_dir: setup.common_dir.clone(),
            kind,
        }
    }

    fn log_path(&self, name: &str) -> PathBuf {
        let per_worktree = !name.starts_with("refs/")
            || name.starts_with("refs/bisect/")
            || name.starts_with("refs/worktree/")
            || name.starts_with("refs/rewritten/");
        let base = if per_worktree {
            &self.git_dir
        } else {
            &self.common_dir
        };
        base.join("logs").join(name)
    }
}

fn lookup_err(e: impl std::fmt::Display) -> LookupError {
    LookupError(e.to_string())
}

/// One reflog line: `<old> <new> <ident> <time> <tz>\t<message>`.
fn parse_reflog_line(kind: HashKind, line: &str) -> Option<ReflogEntry> {
    let (head, message) = line.split_once('\t').unwrap_or((line, ""));
    let hexlen = kind.hex_len();
    let old = ObjectId::from_hex(kind, head.get(..hexlen)?.as_bytes()).ok()?;
    let new = ObjectId::from_hex(kind, head.get(hexlen + 1..2 * hexlen + 1)?.as_bytes()).ok()?;
    let rest = head.get(2 * hexlen + 2..)?;
    let after_email = &rest[rest.rfind('>')? + 1..];
    let mut parts = after_email.split_whitespace();
    let time: u64 = parts.next()?.parse().ok()?;
    let tz: i32 = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0);
    Some(ReflogEntry {
        old,
        new,
        time,
        tz,
        message: message.to_string(),
    })
}

impl RefLookup for Refs {
    fn resolve(&self, name: &str) -> Result<Option<RefValue>, LookupError> {
        let r = self.store.resolve(name).map_err(lookup_err)?;
        Ok(r.map(|r| RefValue {
            name: r.final_name().to_string(),
            oid: r.oid,
        }))
    }

    fn for_each_ref(
        &self,
        prefix: &str,
        f: &mut dyn FnMut(&str, &ObjectId),
    ) -> Result<(), LookupError> {
        let items = self
            .store
            .iter(&IterOptions::prefix(prefix))
            .map_err(lookup_err)?;
        for item in items {
            if let IterItem::Ref(r) = item {
                if let Some(oid) = r.oid {
                    f(&r.name, &oid);
                }
            }
        }
        Ok(())
    }

    fn reflog(&self, name: &str) -> Result<Option<Vec<ReflogEntry>>, LookupError> {
        let data = match std::fs::read(self.log_path(name)) {
            Ok(d) => d,
            Err(_) => return Ok(None),
        };
        let text = String::from_utf8_lossy(&data);
        Ok(Some(
            text.lines()
                .filter_map(|l| parse_reflog_line(self.kind, l))
                .collect(),
        ))
    }
}

/// The configuration as a [`ConfigLookup`].
pub struct Config<'a>(pub &'a ConfigSet);

impl ConfigLookup for Config<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .get(key)
            .and_then(|e| e.value.as_ref())
            .map(|v| v.to_string())
    }

    fn get_all(&self, key: &str) -> Vec<String> {
        self.0
            .get_all(key)
            .filter_map(|e| e.value.as_ref().map(|v| v.to_string()))
            .collect()
    }
}

/// The index, read on first use.
pub struct IndexFile {
    path: PathBuf,
    kind: HashKind,
    index: OnceCell<Option<Index>>,
}

impl IndexFile {
    pub fn new(setup: &Setup, kind: HashKind) -> IndexFile {
        let path = std::env::var_os("GIT_INDEX_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| setup.git_dir.join("index"));
        IndexFile {
            path,
            kind,
            index: OnceCell::new(),
        }
    }
}

impl IndexLookup for IndexFile {
    fn entry(&self, path: &[u8], stage: u8) -> Option<ObjectId> {
        let index = self
            .index
            .get_or_init(|| Index::read_file(&self.path, self.kind, &ReadOptions::default()).ok());
        let stage = match stage {
            1 => Stage::BASE,
            2 => Stage::OURS,
            3 => Stage::THEIRS,
            _ => Stage::MERGED,
        };
        index.as_ref()?.get(path, stage).map(|e| e.id)
    }
}

/// Files in the work tree.
pub struct WorkTree(pub PathBuf);

impl WorkTreeLookup for WorkTree {
    fn exists(&self, path: &str) -> bool {
        std::fs::symlink_metadata(self.0.join(path)).is_ok()
    }
}

/// Where the command runs in the work tree.
pub fn work_tree_info(setup: &Setup) -> Option<WorkTreeInfo> {
    let wt = setup.work_tree.as_ref()?;
    let prefix = setup
        .prefix
        .as_ref()
        .map(|p| {
            let mut s = p.to_string_lossy().into_owned();
            if !s.is_empty() && !s.ends_with('/') {
                s.push('/');
            }
            s
        })
        .unwrap_or_default();
    Some(WorkTreeInfo {
        path: wt.display().to_string(),
        prefix,
        cwd_inside: setup.is_inside_work_tree(),
    })
}

/// The object directory (`GIT_OBJECT_DIRECTORY` or `<common dir>/objects`).
pub fn objects_dir(setup: &Setup) -> PathBuf {
    std::env::var_os("GIT_OBJECT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| setup.common_dir.join("objects"))
}

/// Load the configuration of the repository set up in `setup`
/// (system, global, local, worktree, command line).
pub fn load_config(ctx: &crate::Context, setup: &Setup) -> Result<ConfigSet, Die> {
    use rgit_config::sources::{RepoConfig, ScopeFilter, load_standard};
    let env = rgit_config::SystemEnv::default();
    let abs = |p: &std::path::Path| std::path::absolute(p).ok();
    let branch = FilesStore::new(&setup.git_dir, setup.format.object_format)
        .read_raw("HEAD")
        .ok()
        .flatten()
        .and_then(|t| t.as_symbolic().map(|s| s.to_string()))
        .and_then(|s| s.strip_prefix("refs/heads/").map(|b| b.into()));
    let facts = rgit_config::StaticRepoFacts {
        git_dir: abs(&setup.git_dir),
        worktree: setup.work_tree.clone(),
        branch,
    };
    let opts = rgit_config::LoadOptions {
        env: &env,
        repo: Some(&facts),
        includes: true,
    };
    let repo_cfg = RepoConfig {
        common_dir: setup.common_dir.clone(),
        git_dir: setup.git_dir.clone(),
        worktree_config: Some(setup.format.worktree_config),
    };
    let mut set = ConfigSet::new();
    load_standard(
        opts,
        Some(&repo_cfg),
        &crate::repo::command_line(ctx),
        ScopeFilter::default(),
        &mut set,
    )
    .map_err(|e| {
        use std::io::Write;
        let mut err = std::io::stderr();
        for (level, msg) in e.report() {
            let _ = writeln!(err, "{} {msg}", level.prefix());
        }
        Die::Exit(rgit_options::EXIT_FATAL)
    })?;
    Ok(set)
}

/// The fatal text for an unusable configuration value.
fn unable_to_parse(key: &str, e: &rgit_config::Entry) -> String {
    match &e.source.origin {
        Origin::File(p) => {
            crate::messages::bad_config_variable(key, &p.display().to_string(), e.line)
        }
        Origin::CommandLine => crate::messages::unable_to_parse_config(key, "command-line config"),
        o => crate::messages::unable_to_parse_config(key, &o.describe()),
    }
}

/// Read the revision settings from the configuration. Invalid values end
/// the command like git does.
pub fn options(config: &ConfigSet, kind: HashKind) -> Result<Options, Die> {
    let mut o = Options::default();
    let bool_of = |key: &str| -> Result<Option<bool>, Die> {
        config.bool(key).map_err(|e| Die::Fatal(e.to_string()))
    };
    if let Some(b) = bool_of("core.warnAmbiguousRefs")? {
        o.warn_ambiguous_refs = b;
    }
    if let Some(b) = bool_of("advice.objectNameWarning")? {
        o.advice_object_name_warning = b;
    }
    if let Some(e) = config.get("core.disambiguate") {
        let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
        match Hint::from_config(&v) {
            Some(h) => o.disambiguate = h,
            None => {
                crate::messages::print_error(&crate::messages::unknown_hint_type(&v));
                return Err(Die::Fatal(unable_to_parse("core.disambiguate", e)));
            }
        }
    }
    if let Some(e) = config.get("core.abbrev") {
        let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
        let hexsz = kind.hex_len();
        if v.eq_ignore_ascii_case("auto") {
            o.abbrev = None;
        } else if matches!(v.to_ascii_lowercase().as_str(), "no" | "false" | "off") {
            o.abbrev = Some(hexsz);
        } else {
            let n = e
                .int(rgit_config::IntWidth::I32)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if n < 4 {
                crate::messages::print_error(&crate::messages::abbrev_out_of_range(n));
                return Err(Die::Fatal(unable_to_parse("core.abbrev", e)));
            }
            o.abbrev = Some((n as usize).min(hexsz));
        }
    }
    if let Some(e) = config.get("push.default") {
        let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
        match PushDefault::from_config(&v) {
            Some(p) => o.push_default = p,
            None => {
                crate::messages::print_error(&crate::messages::malformed_push_default(&v));
                crate::messages::print_error(crate::messages::PUSH_DEFAULT_VALUES);
                return Err(Die::Fatal(unable_to_parse("push.default", e)));
            }
        }
    }
    Ok(o)
}
