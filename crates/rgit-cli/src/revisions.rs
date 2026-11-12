// SPDX-License-Identifier: MIT
//! The repository behind the revision parser (`rgit_revwalk::revision`):
//! adapters from refs, objects, index, configuration and work tree to its
//! lookup traits, plus the settings it reads from the configuration.
//!
//! Objects come from `rgit-odb` (loose objects, packs and alternates).
//! Reflogs are read from `logs/` directly until #30 provides them.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::path::PathBuf;

use rgit_config::{ConfigSet, Origin};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_index::{Index, ReadOptions, Stage};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, ObjectDatabase};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;
use rgit_revwalk::revision::{
    ConfigLookup, Hint, IndexLookup, LookupError, ObjectLookup, Options, PushDefault, RefLookup,
    RefValue, ReflogEntry, WorkTreeInfo, WorkTreeLookup,
};

use crate::Die;

/// The object database (loose objects and packs) for the revision parser.
pub struct Objects {
    odb: ObjectDatabase,
    dir: PathBuf,
    count: OnceCell<u64>,
    shallow: HashSet<ObjectId>,
}

impl Objects {
    /// The objects in `dir`; `shallow` is the repository's `shallow` file.
    pub fn new(dir: PathBuf, kind: HashKind, shallow: &std::path::Path) -> Objects {
        let odb = ObjectDatabase::open_with_packs(&dir, kind, LooseOptions::default());
        let shallow = std::fs::read_to_string(shallow)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| ObjectId::from_hex(kind, l.trim().as_bytes()).ok())
            .collect();
        Objects {
            odb,
            dir,
            count: OnceCell::new(),
            shallow,
        }
    }
}

/// An object database error as git reports it: an invalid type ends the
/// command, other corruption makes only this object unusable.
fn odb_error(e: rgit_odb::Error) -> LookupError {
    match e {
        rgit_odb::Error::InvalidType { .. } | rgit_odb::Error::Config(_) => {
            LookupError::fatal(e.to_string())
        }
        e => LookupError::corrupt(e.git_lines()),
    }
}

impl ObjectLookup for Objects {
    fn hash_kind(&self) -> HashKind {
        self.odb.hash_kind()
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError> {
        match self.odb.read(oid) {
            Ok(r) => Ok(r.map(|r| (r.kind, r.data))),
            Err(e) => Err(odb_error(e)),
        }
    }

    fn kind(&self, oid: &ObjectId) -> Result<Option<ObjectKind>, LookupError> {
        match self.odb.header(oid) {
            Ok(h) => Ok(h.map(|h| h.kind)),
            Err(e) => Err(odb_error(e)),
        }
    }

    fn prefix_matches(&self, prefix: &Prefix) -> Result<Vec<ObjectId>, LookupError> {
        let mut out = Vec::new();
        for store in self.odb.stores() {
            store
                .for_each_prefix_match(prefix, &mut |id| out.push(id))
                .map_err(odb_error)?;
        }
        Ok(out)
    }

    fn is_shallow(&self, oid: &ObjectId) -> bool {
        self.shallow.contains(oid)
    }

    fn approximate_count(&self) -> u64 {
        *self.count.get_or_init(|| {
            let mut n: u64 = self
                .odb
                .packs()
                .map(|p| p.set().packs())
                .unwrap_or_default()
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
///
/// In a linked work tree, `HEAD` and the other per-worktree references
/// live in its own git directory and everything else in the common
/// directory (`rgit-refs` does not split them yet, #104).
pub struct Refs {
    store: FilesStore,
    /// The common directory's store when it differs from `store`.
    common: Option<FilesStore>,
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: HashKind,
}

impl Refs {
    pub fn new(setup: &Setup, kind: HashKind) -> Refs {
        let linked = setup.git_dir != setup.common_dir;
        Refs {
            store: FilesStore::new(&setup.git_dir, kind),
            common: linked.then(|| FilesStore::new(&setup.common_dir, kind)),
            git_dir: setup.git_dir.clone(),
            common_dir: setup.common_dir.clone(),
            kind,
        }
    }

    fn store_for(&self, name: &str) -> &FilesStore {
        match &self.common {
            Some(common) if !per_worktree(name) => common,
            _ => &self.store,
        }
    }

    fn log_path(&self, name: &str) -> PathBuf {
        let base = if per_worktree(name) {
            &self.git_dir
        } else {
            &self.common_dir
        };
        base.join("logs").join(name)
    }
}

/// References that belong to one work tree.
fn per_worktree(name: &str) -> bool {
    !name.starts_with("refs/")
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
        || name.starts_with("refs/rewritten/")
}

fn lookup_err(e: impl std::fmt::Display) -> LookupError {
    LookupError::fatal(e.to_string())
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
        if self.common.is_some() {
            // Follow symbolic references across the two stores.
            let mut current = name.to_string();
            for depth in 0..rgit_refs::MAX_SYMREF_DEPTH {
                match self
                    .store_for(&current)
                    .read_raw(&current)
                    .map_err(lookup_err)?
                {
                    Some(rgit_refs::Target::Object(oid)) => {
                        return Ok(Some(RefValue {
                            name: current,
                            oid: Some(oid),
                        }));
                    }
                    Some(rgit_refs::Target::Symbolic(next)) => current = next,
                    None if depth == 0 => return Ok(None),
                    None => {
                        return Ok(Some(RefValue {
                            name: current,
                            oid: None,
                        }));
                    }
                }
            }
            return Ok(None);
        }
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
            .store_for(prefix)
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
