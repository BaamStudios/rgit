// SPDX-License-Identifier: MIT
//! Object access for commands: the repository's object database (loose
//! objects, packs, alternates), its configuration, replace refs and the
//! per-copy details `cat-file` reports (`%(objectsize:disk)`,
//! `%(deltabase)`).
//!
//! The pack store adapter below plugs [`rgit_pack::PackSet`] into
//! [`rgit_odb::ObjectStore`]; it can move into a library crate once more
//! commands need it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rgit_config::{CommandLine, ConfigSet, LoadOptions, SystemEnv, sources};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{ObjectHeader, ObjectKind};
use rgit_odb::{LooseOptions, LooseStore, ObjectDatabase, ObjectReader, ObjectStore, RawObject};
use rgit_pack::{DeltaBaseCache, PackSet};
use rgit_repo::Setup;

use crate::refs::Refs;
use crate::{Context, Die};

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

    /// A copy in a cruft pack does not count (see `PackSet::freshen`).
    fn freshen(&self, oid: &ObjectId) -> bool {
        self.set.freshen(oid)
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
        self.read_with_bases(oid, &mut |_| {}, &mut |_| None)
    }

    fn read_with_bases(
        &self,
        oid: &ObjectId,
        _report: &mut dyn FnMut(&rgit_odb::Error),
        bases: &mut dyn FnMut(&ObjectId) -> Option<RawObject>,
    ) -> Result<Option<RawObject>, rgit_odb::Error> {
        rgit_odb::check_pack_alloc(&self.set, oid)?;
        let mut cache = self.cache.lock().expect("cache lock");
        let obj = self
            .set
            .read_with_fallback(oid, &mut cache, &mut |_| {}, bases)
            .map_err(store_err)?;
        Ok(obj.map(|o| RawObject {
            kind: kind_of(o.kind),
            data: o.data,
        }))
    }

    fn stream(&self, oid: &ObjectId) -> Result<Option<ObjectReader>, rgit_odb::Error> {
        if !self.contains(oid) {
            return Ok(None);
        }
        match rgit_odb::stream_from_packs(&self.set, oid) {
            Some(r) => Ok(Some(r)),
            None => Ok(self.read(oid)?.map(ObjectReader::from_raw)),
        }
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
    /// What hooks inherit from the command line (#76).
    pub hook_context: crate::hooks::HookContext,
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
        odb.set_large_object_options(
            rgit_odb::LargeObjectOptions::from_config(&config)
                .map_err(|e| Die::Fatal(e.to_string()))?,
        );
        let mut dirs = Vec::new();
        let use_midx = config
            .bool("core.multiPackIndex")
            .ok()
            .flatten()
            .unwrap_or(true);
        let main_packs = Arc::new(PackStore::new(Arc::new(crate::repo::pack_set(
            objects.join("pack"),
            hash,
            use_midx,
        )?)));
        odb.push_store(main_packs.clone());
        dirs.push(ObjectDir {
            loose: Arc::new(LooseStore::new(&objects, hash, loose_opts.clone())),
            packs: main_packs,
        });
        let alternates = rgit_odb::resolve_alternates_reporting(&objects, &mut |e| {
            eprintln!("error: {e}");
        });
        for dir in alternates {
            let loose = Arc::new(LooseStore::new(&dir, hash, loose_opts.clone()));
            let packs = Arc::new(PackStore::new(Arc::new(crate::repo::pack_set(
                dir.join("pack"),
                hash,
                use_midx,
            )?)));
            odb.push_store(loose.clone());
            odb.push_store(packs.clone());
            dirs.push(ObjectDir { loose, packs });
        }
        if !ctx.no_replace_objects {
            crate::replace_objects::install(&mut odb, &setup, &config);
        }
        let refs = Refs::new(&setup);
        let repo = Repo {
            setup,
            config,
            hash,
            odb,
            dirs,
            refs,
            hook_context: crate::hooks::HookContext::new(ctx),
        };
        crate::hooks::watch_index(&repo);
        Ok(repo)
    }

    /// The replacement of `oid` (following chains), or `oid` itself.
    /// A chain that is too long or loops ends the command, as in git.
    pub fn replaced(&mut self, oid: &ObjectId) -> ObjectId {
        match self.odb.replaced(oid) {
            Ok(id) => id,
            Err(e) => crate::convert::die_now(&e.to_string()),
        }
    }

    /// Replace refs as `original -> replacement` (empty when replacing is
    /// disabled).
    pub fn replacements(&mut self) -> &HashMap<ObjectId, ObjectId> {
        static EMPTY: std::sync::OnceLock<HashMap<ObjectId, ObjectId>> = std::sync::OnceLock::new();
        match self.odb.replacements() {
            Some(m) => m,
            None => EMPTY.get_or_init(HashMap::new),
        }
    }

    /// Stop following replace refs (commands that check or transfer the
    /// stored objects).
    pub fn no_replace(&mut self) {
        self.odb.clear_replacements();
    }

    /// Read the legacy graft file now, if there is one (printing its
    /// deprecation hint), as git does when a command parses commits.
    pub fn read_grafts(&self) {
        crate::grafts::Lazy::new(&self.setup.common_dir, self.hash, &self.config).touch();
    }

    /// [`Repo::replacements`] as an owned value, for lookups without a
    /// mutable repository.
    pub fn replacement_lookup(&mut self) -> Replacements {
        Replacements(self.replacements().clone())
    }

    /// The alternate object directories in lookup order (without the
    /// repository's own).
    pub fn alternate_dirs(&self) -> Vec<PathBuf> {
        self.dirs[1..]
            .iter()
            .map(|d| d.loose.objects_dir().to_path_buf())
            .collect()
    }

    /// Whether a pack (local or of an alternate) holds `oid`.
    pub fn is_packed(&self, oid: &ObjectId) -> bool {
        self.dirs.iter().any(|d| d.packs.contains(oid))
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

    /// Whether a pack with a `.keep` file holds `oid`.
    pub fn in_kept_pack(&self, oid: &ObjectId) -> bool {
        self.dirs.iter().any(|d| {
            d.packs
                .set
                .packs()
                .iter()
                .any(|p| p.is_keep() && p.contains(oid))
        })
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

/// The objects directory of a repository (`GIT_OBJECT_DIRECTORY` or
/// `<common dir>/objects`).
pub fn objects_dir(setup: &Setup) -> PathBuf {
    rgit_odb::objects_dir(&setup.common_dir)
}

/// Replace refs: object to replacement.
#[derive(Debug, Clone, Default)]
pub struct Replacements(HashMap<ObjectId, ObjectId>);

impl Replacements {
    /// The replacement of `oid` (following chains), or `oid` itself.
    pub fn get(&self, oid: &ObjectId) -> ObjectId {
        chain(&self.0, oid)
    }
}

fn chain(map: &HashMap<ObjectId, ObjectId>, oid: &ObjectId) -> ObjectId {
    match rgit_odb::follow_replacements(map, oid) {
        Ok(id) => id,
        Err(e) => crate::convert::die_now(&e.to_string()),
    }
}
