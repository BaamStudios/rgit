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

use crate::{Context, Die};

/// The object database (loose objects and packs) for the revision parser.
pub struct Objects {
    odb: ObjectDatabase,
    dir: PathBuf,
    count: OnceCell<u64>,
    shallow: HashSet<ObjectId>,
    /// Replacements (`refs/replace/<id>`), see [`Objects::use_replace_refs`].
    replace: std::collections::HashMap<ObjectId, ObjectId>,
    replacing: bool,
}

impl Objects {
    /// Read replaced objects through their replacements (git-replace(1)):
    /// the refs under `refs/replace/` (`$GIT_REPLACE_REF_BASE`), unless
    /// `disabled` (`--no-replace-objects`), `$GIT_NO_REPLACE_OBJECTS` or
    /// `core.useReplaceRefs=false` turn that off.
    pub fn use_replace_refs(&mut self, refs: &Refs, config: &ConfigSet, disabled: bool) {
        if disabled
            || std::env::var_os("GIT_NO_REPLACE_OBJECTS").is_some()
            || config.bool("core.useReplaceRefs").ok().flatten() == Some(false)
        {
            return;
        }
        self.replacing = true;
        let base = std::env::var("GIT_REPLACE_REF_BASE")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "refs/replace/".to_owned());
        let kind = self.odb.hash_kind();
        let mut map = std::collections::HashMap::new();
        let _ = refs.for_each_ref(&base, &mut |name, target| {
            if let Some(hex) = name.strip_prefix(base.as_str()) {
                if let Ok(id) = ObjectId::from_hex(kind, hex.as_bytes()) {
                    map.insert(id, *target);
                }
            }
        });
        self.replace = map;
    }

    /// Whether replace refs are in use (see [`Objects::use_replace_refs`]).
    pub fn replacing(&self) -> bool {
        self.replacing
    }

    /// `oid` after following its replacements.
    fn replaced<'a>(&'a self, mut oid: &'a ObjectId) -> &'a ObjectId {
        for _ in 0..16 {
            match self.replace.get(oid) {
                Some(next) if next != oid => oid = next,
                _ => break,
            }
        }
        oid
    }

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
            replace: std::collections::HashMap::new(),
            replacing: false,
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
        match self.odb.read(self.replaced(oid)) {
            Ok(r) => Ok(r.map(|r| (r.kind, r.data))),
            Err(e) => Err(odb_error(e)),
        }
    }

    fn kind(&self, oid: &ObjectId) -> Result<Option<ObjectKind>, LookupError> {
        match self.odb.header(self.replaced(oid)) {
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
/// directory; the store tells them apart.
pub struct Refs {
    store: FilesStore,
    git_dir: PathBuf,
    common_dir: PathBuf,
    kind: HashKind,
}

impl Refs {
    pub fn new(setup: &Setup, kind: HashKind) -> Refs {
        Refs {
            store: FilesStore::new(&setup.common_dir, kind).with_worktree(&setup.git_dir),
            git_dir: setup.git_dir.clone(),
            common_dir: setup.common_dir.clone(),
            kind,
        }
    }

    fn store_for(&self, _name: &str) -> &FilesStore {
        &self.store
    }

    fn log_path(&self, name: &str) -> PathBuf {
        rgit_refs::reflog::FilesReflog::new(&self.git_dir, &self.common_dir, self.kind).path(name)
    }
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

    fn entries_with_prefix(&self, prefix: &str) -> Vec<(String, String)> {
        self.0
            .entries()
            .iter()
            .filter(|e| e.name.starts_with(prefix.as_bytes()))
            .filter_map(|e| Some((e.name.to_string(), e.value.as_ref()?.to_string())))
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
pub(crate) fn unable_to_parse(key: &str, e: &rgit_config::Entry) -> String {
    match &e.source.origin {
        Origin::File(p) => {
            crate::messages::bad_config_variable(key, &p.display().to_string(), e.line)
        }
        Origin::CommandLine => crate::messages::unable_to_parse_config(key, "command-line config"),
        o => crate::messages::unable_to_parse_config(key, &o.describe()),
    }
}

/// The revision settings from the configuration, ignoring invalid values
/// (for lookups inside commands that do not report them).
pub fn options_lenient(config: &ConfigSet, kind: HashKind) -> Options {
    let mut o = Options::default();
    if let Ok(Some(b)) = config.bool("core.warnAmbiguousRefs") {
        o.warn_ambiguous_refs = b;
    }
    let value = |key: &str| {
        config
            .get(key)
            .and_then(|e| e.value.as_ref().map(|v| v.to_string()))
    };
    if let Some(h) = value("core.disambiguate").and_then(|v| Hint::from_config(&v)) {
        o.disambiguate = h;
    }
    if let Some(v) = value("core.abbrev") {
        if v.eq_ignore_ascii_case("auto") {
            o.abbrev = None;
        } else if let Ok(n) = v.parse::<usize>() {
            o.abbrev = Some(n.clamp(4, kind.hex_len()));
        }
    }
    if let Some(p) = value("push.default").and_then(|v| PushDefault::from_config(&v)) {
        o.push_default = p;
    }
    o
}

/// Read the revision settings from the configuration. Invalid values end
/// the command like git does.
pub fn options(ctx: &Context, config: &ConfigSet, kind: HashKind) -> Result<Options, Die> {
    let mut o = Options::default();
    let bool_of = |key: &str| -> Result<Option<bool>, Die> {
        config.bool(key).map_err(|e| Die::Fatal(e.to_string()))
    };
    if let Some(b) = bool_of("core.warnAmbiguousRefs")? {
        o.warn_ambiguous_refs = b;
    }
    // An invalid `GIT_ADVICE` is not reported here (git reports it only
    // when the warning is about to be shown); the advice then stays on.
    o.advice_object_name_warning = ctx
        .advisor(config)?
        .enabled(rgit_messages::Advice::ObjectNameWarning)
        .unwrap_or(true);
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

/// What `config --blob=<name>` finds.
#[derive(Debug, Clone)]
pub enum BlobLookup {
    /// No repository, or `name` does not name an object.
    Unresolved,
    /// `name` names an object that is not a blob.
    NotBlob,
    /// The blob's content.
    Blob(Vec<u8>),
}

/// Resolve `name` in the repository of the current directory and read it
/// as a blob. `prefix` is the original directory relative to the top of
/// the work tree (`HEAD:./file`); the caller may already have moved to the
/// top. Nothing is printed: the caller reports problems like git.
pub fn read_blob(ctx: &crate::Context, name: &str, prefix: &str) -> BlobLookup {
    let env = rgit_config::SystemEnv::default();
    let Ok(mut opts) = rgit_repo::SetupOptions::from_process() else {
        return BlobLookup::Unresolved;
    };
    if let Some(d) = &ctx.git_dir {
        opts.git_dir = Some(d.into());
    }
    if let Some(w) = &ctx.work_tree {
        opts.work_tree = Some(w.into());
    }
    if ctx.bare {
        opts.implicit_work_tree = false;
    }
    let mut protected =
        rgit_repo::StandardProtectedConfig::new(&env, crate::repo::command_line(ctx));
    let Ok(setup) = rgit_repo::setup(&opts, &mut protected) else {
        return BlobLookup::Unresolved;
    };
    let kind = setup.format.object_format;
    let shallow = setup.common_dir.join("shallow");
    let objects = Objects::new(objects_dir(&setup), kind, &shallow);
    let refs = Refs::new(&setup, kind);
    let config = ConfigSet::new();
    let cfg = Config(&config);
    let index = IndexFile::new(&setup, kind);
    let work_tree = setup.work_tree.clone().map(WorkTree);
    let repo = rgit_revwalk::revision::Repo {
        objects: &objects,
        refs: &refs,
        config: &cfg,
        index: Some(&index),
        work_tree: work_tree.as_ref().map(|w| w as &dyn WorkTreeLookup),
        work_tree_info: work_tree_info(&setup).map(|mut info| {
            if !prefix.is_empty() {
                info.prefix = prefix.to_string();
                if !info.prefix.ends_with('/') {
                    info.prefix.push('/');
                }
                info.cwd_inside = true;
            }
            info
        }),
    };
    let resolver = rgit_revwalk::revision::Resolver::new(repo, Options::default());
    let Ok(oid) = resolver.resolve(name) else {
        return BlobLookup::Unresolved;
    };
    match objects.read(&oid) {
        Ok(Some((ObjectKind::Blob, data))) => BlobLookup::Blob(data),
        Ok(Some(_)) => BlobLookup::NotBlob,
        _ => BlobLookup::Unresolved,
    }
}
