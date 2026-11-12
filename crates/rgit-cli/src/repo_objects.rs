// SPDX-License-Identifier: MIT
//! Object access for commands: the repository's object database (loose
//! objects, packs, alternates), its configuration, replace refs and the
//! per-copy details `cat-file` reports (`%(objectsize:disk)`,
//! `%(deltabase)`).
//!
//! The pack store adapter below plugs [`rgit_pack::PackSet`] into
//! [`rgit_odb::ObjectStore`]; it can move into a library crate once more
//! commands need it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rgit_config::{CommandLine, ConfigSet, LoadOptions, SystemEnv, sources};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{ObjectHeader, ObjectKind};
use rgit_odb::{LooseOptions, LooseStore, ObjectDatabase, ObjectStore, RawObject};
use rgit_pack::{DeltaBaseCache, PackSet};
use rgit_repo::Setup;

use crate::refs::Refs;
use crate::{Context, Die};

/// Alternates are followed this many levels deep.
const MAX_ALTERNATE_DEPTH: usize = 5;

/// Replacements are followed this many times.
const MAX_REPLACE_DEPTH: usize = 5;

/// A pack directory as an [`ObjectStore`].
#[derive(Debug)]
pub struct PackStore {
    set: Arc<PackSet>,
    cache: Mutex<DeltaBaseCache>,
}

impl PackStore {
    pub fn new(set: Arc<PackSet>) -> PackStore {
        PackStore {
            set,
            cache: Mutex::new(DeltaBaseCache::new(
                rgit_pack::DEFAULT_DELTA_BASE_CACHE_LIMIT,
            )),
        }
    }

    /// The pack holding `oid` and the offset of its entry.
    fn locate(&self, oid: &ObjectId) -> Option<(Arc<rgit_pack::Pack>, u64)> {
        for pack in self.set.packs() {
            if let Some(pos) = pack.index().lookup(oid) {
                if let Ok(off) = pack.index().offset_at(pos) {
                    return Some((pack, off));
                }
            }
        }
        None
    }
}

fn kind_of(k: rgit_pack::ObjectKind) -> ObjectKind {
    ObjectKind::from_pack_type(k.pack_type()).expect("pack kinds are object kinds")
}

fn store_err(e: rgit_pack::Error) -> rgit_odb::Error {
    rgit_odb::Error::Store(Box::new(e))
}

impl ObjectStore for PackStore {
    fn hash_kind(&self) -> HashKind {
        self.set.hash_kind()
    }

    fn contains(&self, oid: &ObjectId) -> bool {
        self.set.contains(oid) || (self.set.rescan_if_changed() && self.set.contains(oid))
    }

    fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, rgit_odb::Error> {
        if !self.contains(oid) {
            return Ok(None);
        }
        if let Some((pack, off)) = self.locate(oid) {
            if let Ok((kind, size)) = pack.object_info(off) {
                return Ok(Some(ObjectHeader {
                    kind: kind_of(kind),
                    size,
                }));
            }
        }
        Ok(self.read(oid)?.map(|o| o.header()))
    }

    fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, rgit_odb::Error> {
        let mut cache = self.cache.lock().expect("cache lock");
        let obj = self
            .set
            .read(oid, &mut cache, &mut |_| {})
            .map_err(store_err)?;
        Ok(obj.map(|o| RawObject {
            kind: kind_of(o.kind),
            data: o.data,
        }))
    }

    fn for_each_prefix_match(
        &self,
        prefix: &Prefix,
        f: &mut dyn FnMut(ObjectId),
    ) -> Result<(), rgit_odb::Error> {
        for id in self.set.lookup_prefix(prefix, usize::MAX) {
            f(id);
        }
        Ok(())
    }

    fn for_each_object(&self, f: &mut dyn FnMut(ObjectId)) -> Result<(), rgit_odb::Error> {
        for id in self.set.all_ids() {
            f(id);
        }
        Ok(())
    }
}

/// One object directory: its loose objects and packs.
struct ObjectDir {
    loose: Arc<LooseStore>,
    packs: Arc<PackStore>,
}

/// Where a copy of an object is stored.
pub enum Location {
    Loose(PathBuf),
    Packed(Arc<rgit_pack::Pack>, u64),
}

/// A repository opened for object access.
pub struct Repo {
    pub setup: Setup,
    pub config: ConfigSet,
    pub hash: HashKind,
    pub odb: ObjectDatabase,
    dirs: Vec<ObjectDir>,
    pub refs: Refs,
    replace_enabled: bool,
    replace: Option<HashMap<ObjectId, ObjectId>>,
}

/// Load the configuration (system, global, local, worktree, command line).
pub fn load_config(ctx: &Context, setup: Option<&Setup>) -> Result<ConfigSet, Die> {
    let env = SystemEnv::default();
    let opts = LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    };
    let repo_cfg = setup.map(|s| sources::RepoConfig {
        common_dir: s.common_dir.clone(),
        git_dir: s.git_dir.clone(),
        worktree_config: None,
    });
    let cl: CommandLine = crate::repo::command_line(ctx);
    let mut set = ConfigSet::new();
    match sources::load_standard(
        opts,
        repo_cfg.as_ref(),
        &cl,
        sources::ScopeFilter::default(),
        &mut set,
    ) {
        Ok(()) => Ok(set),
        Err(e) => {
            let mut lines = e.report();
            let last = lines.pop();
            for (level, m) in lines {
                eprintln!("{} {m}", level.prefix());
            }
            match last {
                Some((_, m)) => Err(Die::Fatal(m)),
                None => Err(Die::Exit(128)),
            }
        }
    }
}

/// `true`/`yes`/`on`/`1` (git's environment boolean), with a default.
pub fn env_bool(name: &str, default: bool) -> bool {
    match std::env::var(name) {
        Ok(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => true,
            "false" | "no" | "off" | "0" | "" => false,
            _ => default,
        },
        Err(_) => default,
    }
}

impl Repo {
    /// Open the object database of a set-up repository.
    pub fn open(ctx: &Context, setup: Setup) -> Result<Repo, Die> {
        let config = load_config(ctx, Some(&setup))?;
        let hash = setup.format.object_format;
        let mut warnings = Vec::new();
        let loose_opts = LooseOptions::from_config(&config, &mut warnings)
            .map_err(|e| Die::Fatal(e.to_string()))?;
        for w in warnings {
            eprintln!("warning: {w}");
        }
        let objects = objects_dir(&setup);
        let mut odb = ObjectDatabase::open(&objects, hash, loose_opts.clone());
        let mut dirs = Vec::new();
        let main_packs = Arc::new(PackStore::new(Arc::new(PackSet::open(
            objects.join("pack"),
            hash,
        ))));
        odb.push_store(main_packs.clone());
        dirs.push(ObjectDir {
            loose: Arc::new(LooseStore::new(&objects, hash, loose_opts.clone())),
            packs: main_packs,
        });
        let mut seen = BTreeSet::new();
        seen.insert(canonical(&objects));
        let mut alts = Vec::new();
        if let Ok(env) = std::env::var("GIT_ALTERNATE_OBJECT_DIRECTORIES") {
            let cwd = std::env::current_dir().unwrap_or_default();
            for p in std::env::split_paths(&env) {
                if !p.as_os_str().is_empty() {
                    alts.push((cwd.join(p), 0));
                }
            }
        }
        for p in read_alternates(&objects) {
            alts.push((p, 1));
        }
        let mut i = 0;
        while i < alts.len() {
            let (dir, depth) = alts[i].clone();
            i += 1;
            if !dir.is_dir() || !seen.insert(canonical(&dir)) {
                continue;
            }
            let loose = Arc::new(LooseStore::new(&dir, hash, loose_opts.clone()));
            let packs = Arc::new(PackStore::new(Arc::new(PackSet::open(
                dir.join("pack"),
                hash,
            ))));
            odb.push_store(loose.clone());
            odb.push_store(packs.clone());
            dirs.push(ObjectDir { loose, packs });
            if depth < MAX_ALTERNATE_DEPTH {
                for p in read_alternates(&dir) {
                    alts.push((p, depth + 1));
                }
            }
        }
        let replace_enabled = !ctx.no_replace_objects
            && std::env::var_os("GIT_NO_REPLACE_OBJECTS").is_none()
            && config
                .bool("core.useReplaceRefs")
                .ok()
                .flatten()
                .unwrap_or(true);
        let refs = Refs::new(&setup);
        Ok(Repo {
            setup,
            config,
            hash,
            odb,
            dirs,
            refs,
            replace_enabled,
            replace: None,
        })
    }

    /// The replacement of `oid` (following chains), or `oid` itself.
    pub fn replaced(&mut self, oid: &ObjectId) -> ObjectId {
        if !self.replace_enabled {
            return *oid;
        }
        if self.replace.is_none() {
            let base = std::env::var("GIT_REPLACE_REF_BASE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "refs/replace/".to_string());
            let mut map = HashMap::new();
            for (name, target) in self.refs.list(&base) {
                let hex = &name[base.len()..];
                if let Ok(from) = ObjectId::from_hex(self.hash, hex.as_bytes()) {
                    map.insert(from, target);
                }
            }
            self.replace = Some(map);
        }
        let map = self.replace.as_ref().expect("loaded");
        let mut cur = *oid;
        for _ in 0..MAX_REPLACE_DEPTH {
            match map.get(&cur) {
                Some(next) => cur = *next,
                None => break,
            }
        }
        cur
    }

    /// Where the first copy of `oid` lives (loose before packed, local
    /// before alternates, as lookups go).
    pub fn locate(&self, oid: &ObjectId) -> Option<Location> {
        for d in &self.dirs {
            if d.loose.contains(oid) {
                return Some(Location::Loose(d.loose.path_of(oid)));
            }
            if let Some((p, off)) = d.packs.locate(oid) {
                return Some(Location::Packed(p, off));
            }
        }
        None
    }

    /// Bytes the copy of `oid` takes on disk.
    pub fn disk_size(&self, oid: &ObjectId) -> u64 {
        match self.locate(oid) {
            Some(Location::Loose(path)) => std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            Some(Location::Packed(p, off)) => p.entry_end(off).map(|e| e - off).unwrap_or(0),
            None => 0,
        }
    }

    /// The delta base of the stored copy of `oid` (null if not a delta).
    pub fn delta_base(&self, oid: &ObjectId) -> ObjectId {
        match self.locate(oid) {
            Some(Location::Packed(p, off)) => p.delta_base(off).ok().flatten(),
            _ => None,
        }
        .unwrap_or_else(|| self.hash.null())
    }

    /// Number of objects (for the default abbreviation length).
    pub fn approximate_object_count(&self) -> usize {
        let mut n = 0usize;
        for d in &self.dirs {
            for p in d.packs.set.packs() {
                n += p.index().len() as usize;
            }
            let _ = d.loose.for_each_object(&mut |_| n += 1);
        }
        n
    }
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The objects directory of a repository (`GIT_OBJECT_DIRECTORY` or
/// `<common dir>/objects`).
pub fn objects_dir(setup: &Setup) -> PathBuf {
    rgit_odb::objects_dir(&setup.common_dir)
}

/// The entries of `<objects>/info/alternates`, made absolute.
fn read_alternates(objects: &Path) -> Vec<PathBuf> {
    let Ok(data) = std::fs::read(objects.join("info").join("alternates")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in data.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let path = if line[0] == b'"' {
            match crate::quote::c_unquote(line) {
                Some(p) => p,
                None => continue,
            }
        } else {
            line.to_vec()
        };
        let p = PathBuf::from(String::from_utf8_lossy(&path).into_owned());
        out.push(if p.is_absolute() { p } else { objects.join(p) });
    }
    out
}
