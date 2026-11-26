// SPDX-License-Identifier: MIT

//! The object database: an ordered list of stores plus the loose store
//! that new objects are written to.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{Object, ObjectHeader, ObjectKind};

use rgit_config::ConfigSet;

use crate::alternates;
use crate::bulk::{Bulk, BulkOptions};
use crate::packed::PackStore;
use crate::store::{ObjectStore, RawObject};
use crate::stream::{DEFAULT_BIG_FILE_THRESHOLD, ObjectReader};
use crate::{Error, LooseOptions, LooseStore, ZLIB_DEFAULT_COMPRESSION, messages};

/// The environment variable that overrides `$GIT_DIR/objects`.
pub const OBJECT_DIRECTORY_ENV: &str = "GIT_OBJECT_DIRECTORY";

/// The `objects` directory of a repository: `$GIT_OBJECT_DIRECTORY` if set
/// and not empty, else `<git_dir>/objects`.
pub fn objects_dir(git_dir: &Path) -> PathBuf {
    match std::env::var_os(OBJECT_DIRECTORY_ENV) {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => git_dir.join("objects"),
    }
}

/// Result of looking up an abbreviated id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrefixMatch {
    /// No object starts with the prefix.
    None,
    /// Exactly one object does.
    Unique(ObjectId),
    /// Several do (sorted).
    Ambiguous(Vec<ObjectId>),
}

type ErrorSink = Box<dyn Fn(&Error) + Send + Sync>;

/// How large objects are handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LargeObjectOptions {
    /// `core.bigFileThreshold`: blobs above this size are streamed and,
    /// when added, stored deflated in a pack without delta compression.
    pub big_file_threshold: u64,
    /// zlib level for those packs (`pack.compression`, else
    /// `core.compression`; `-1` is zlib's default).
    pub pack_compression: u32,
    /// `pack.packSizeLimit` (0: none).
    pub pack_size_limit: u64,
}

impl Default for LargeObjectOptions {
    fn default() -> LargeObjectOptions {
        LargeObjectOptions {
            big_file_threshold: DEFAULT_BIG_FILE_THRESHOLD,
            pack_compression: ZLIB_DEFAULT_COMPRESSION,
            pack_size_limit: 0,
        }
    }
}

impl LargeObjectOptions {
    /// Read the settings from `config`. A malformed value is an
    /// [`Error::Config`] (git dies).
    pub fn from_config(config: &ConfigSet) -> Result<LargeObjectOptions, Error> {
        let cfg = |e: rgit_config::ValueError| Error::Config(e.to_string());
        let mut o = LargeObjectOptions::default();
        if let Some(v) = config.ulong("core.bigFileThreshold").map_err(cfg)? {
            o.big_file_threshold = v;
        }
        for key in ["pack.compression", "core.compression"] {
            if let Some(v) = config.int(key).map_err(cfg)? {
                o.pack_compression = match v {
                    -1 => ZLIB_DEFAULT_COMPRESSION,
                    0..=9 => v as u32,
                    _ => return Err(Error::Config(messages::bad_compression_level(v))),
                };
                break;
            }
        }
        if let Some(v) = config.ulong("pack.packSizeLimit").map_err(cfg)? {
            o.pack_size_limit = v;
        }
        Ok(o)
    }
}

/// The default error sink: every line of [`Error::skipped_copy_lines`] as
/// `error: <line>` on standard error, like git.
pub fn print_error_to_stderr(e: &Error) {
    use std::io::Write;
    let mut err = std::io::stderr().lock();
    for line in e.skipped_copy_lines() {
        let _ = writeln!(err, "error: {line}");
    }
}

/// Reads objects from a list of stores and writes new ones as loose objects.
///
/// Lookups ask the stores in order. When a store has an object but cannot
/// deliver it intact (a corrupt loose file, say), the next store is tried.
/// If one delivers it, [`ObjectDatabase::read`] passes the errors of the
/// skipped copies to the error sink (git prints them and goes on); if none
/// does, the first error is returned instead and the caller reports it.
/// Broken copies inside a set of packs are passed to the sink as they are
/// skipped. By default the sink prints to standard error as git does
/// ([`print_error_to_stderr`]); [`ObjectDatabase::set_error_sink`]
/// replaces it and [`ObjectDatabase::quiet`] turns it off.
///
/// Commands open the database of a repository with
/// [`ObjectDatabase::open_for_repo`] (loose objects, packs and alternates).
///
/// Object replacement (`git-replace(1)`) is off until
/// [`ObjectDatabase::set_replacements`] installs the replace refs; then
/// [`ObjectDatabase::read`] and [`ObjectDatabase::header`] answer for the
/// replacement of an object (the `_unreplaced` variants do not).
pub struct ObjectDatabase {
    hash: HashKind,
    loose: Arc<LooseStore>,
    packs: Option<Arc<PackStore>>,
    stores: Vec<Arc<dyn ObjectStore>>,
    sink: Option<ErrorSink>,
    large: LargeObjectOptions,
    /// The pack large objects are being written to (see `bulk.rs`).
    bulk: Mutex<Option<Bulk>>,
    /// Whether `bulk` holds objects that are not readable yet.
    bulk_pending: AtomicBool,
    /// Packs of alternates (for [`ObjectDatabase::promisor_objects`]).
    alternate_packs: Vec<Arc<PackStore>>,
    promisor: std::sync::OnceLock<HashSet<ObjectId>>,
    replace: Option<Replacements>,
}

/// Replacements are followed at most this many times; an object whose
/// chain is longer (or loops) cannot be read.
pub const MAX_REPLACE_DEPTH: usize = 5;

/// Loads the replace refs as `original -> replacement`.
pub type ReplaceLoader = Box<dyn Fn() -> HashMap<ObjectId, ObjectId> + Send + Sync>;

/// Replace refs, loaded on first use.
struct Replacements {
    load: ReplaceLoader,
    map: OnceLock<HashMap<ObjectId, ObjectId>>,
}

impl Replacements {
    fn map(&self) -> &HashMap<ObjectId, ObjectId> {
        self.map.get_or_init(|| (self.load)())
    }
}

impl std::fmt::Debug for ObjectDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectDatabase")
            .field("hash", &self.hash)
            .field("stores", &self.stores)
            .finish_non_exhaustive()
    }
}

impl ObjectDatabase {
    /// A database over `objects_dir` containing only its loose store.
    pub fn open(
        objects_dir: impl Into<PathBuf>,
        hash: HashKind,
        options: LooseOptions,
    ) -> ObjectDatabase {
        let loose = Arc::new(LooseStore::new(objects_dir, hash, options));
        ObjectDatabase {
            hash,
            stores: vec![loose.clone()],
            loose,
            packs: None,
            sink: Some(Box::new(print_error_to_stderr)),
            large: LargeObjectOptions::default(),
            bulk: Mutex::new(None),
            bulk_pending: AtomicBool::new(false),
            alternate_packs: Vec::new(),
            promisor: std::sync::OnceLock::new(),
            replace: None,
        }
    }

    /// The full database of an `objects` directory: its loose objects
    /// (where new objects are written), its packs, then the loose objects
    /// and packs of every alternate (`objects/info/alternates`,
    /// recursively, and `$GIT_ALTERNATE_OBJECT_DIRECTORIES`). Loose objects
    /// are asked before packs, as git does when reading an object (a
    /// corrupt loose copy is reported, then the packed copy is used).
    pub fn open_with_packs(
        objects_dir: impl Into<PathBuf>,
        hash: HashKind,
        options: LooseOptions,
    ) -> ObjectDatabase {
        let objects_dir = objects_dir.into();
        let mut db = ObjectDatabase::open(&objects_dir, hash, options.clone());
        let packs = Arc::new(PackStore::open(objects_dir.join("pack"), hash));
        db.push_store(packs.clone());
        db.packs = Some(packs);
        for alt in alternates::resolve(&objects_dir) {
            db.push_store(Arc::new(LooseStore::new(&alt, hash, options.clone())));
            let packs = Arc::new(PackStore::open(alt.join("pack"), hash));
            db.alternate_packs.push(packs.clone());
            db.push_store(packs);
        }
        db
    }

    /// The database of a repository: [`ObjectDatabase::open_with_packs`] on
    /// [`objects_dir`] of its common directory, with the hash algorithm of
    /// the repository, the loose object settings from `config`
    /// ([`LooseOptions::from_config`]) and `core.deltaBaseCacheLimit`.
    ///
    /// Configuration warnings are appended to `warnings` (print each as
    /// `warning: <text>`); invalid values are an [`Error::Config`] (fatal
    /// in git).
    ///
    /// ```no_run
    /// # fn f(setup: &rgit_repo::Setup, config: &rgit_config::ConfigSet) {
    /// let mut warnings = Vec::new();
    /// let odb = rgit_odb::ObjectDatabase::open_for_repo(setup, config, &mut warnings).unwrap();
    /// # }
    /// ```
    pub fn open_for_repo(
        setup: &rgit_repo::Setup,
        config: &ConfigSet,
        warnings: &mut Vec<String>,
    ) -> Result<ObjectDatabase, Error> {
        let options = LooseOptions::from_config(config, warnings)?;
        let large = LargeObjectOptions::from_config(config)?;
        let hash = setup.format.object_format;
        let mut db = ObjectDatabase::open_with_packs(objects_dir(&setup.common_dir), hash, options);
        db.set_large_object_options(large);
        let limit = config.int("core.deltaBaseCacheLimit").ok().flatten();
        if let (Some(limit), Some(packs)) = (limit, &db.packs) {
            packs.set_cache_limit(usize::try_from(limit).unwrap_or(0));
        }
        Ok(db)
    }

    /// The packs of the main `objects` directory, if the database was
    /// opened with them.
    pub fn packs(&self) -> Option<&Arc<PackStore>> {
        self.packs.as_ref()
    }

    /// The hash kind of the repository.
    pub fn hash_kind(&self) -> HashKind {
        self.hash
    }

    /// The loose store new objects are written to.
    pub fn loose(&self) -> &LooseStore {
        &self.loose
    }

    /// The stores in lookup order (the loose store is first initially).
    pub fn stores(&self) -> &[Arc<dyn ObjectStore>] {
        &self.stores
    }

    /// Add a store at the end of the lookup order.
    pub fn push_store(&mut self, store: Arc<dyn ObjectStore>) {
        self.stores.push(store);
    }

    /// Add a store at `index` of the lookup order (0 = asked first, e.g.
    /// packs before loose objects).
    pub fn insert_store(&mut self, index: usize, store: Arc<dyn ObjectStore>) {
        self.stores.insert(index, store);
    }

    /// Receive errors of object copies that are skipped during a lookup
    /// instead of printing them (the default, see
    /// [`print_error_to_stderr`]).
    pub fn set_error_sink(&mut self, sink: impl Fn(&Error) + Send + Sync + 'static) {
        self.sink = Some(Box::new(sink));
    }

    /// Do not report skipped copies at all.
    pub fn quiet(&mut self) {
        self.sink = None;
    }

    /// Read objects through their replacements: `load` returns the
    /// replace refs (`original -> replacement`). It runs once, when the
    /// first object is read, so a command that reads no objects pays
    /// nothing.
    pub fn set_replacements(
        &mut self,
        load: impl Fn() -> HashMap<ObjectId, ObjectId> + Send + Sync + 'static,
    ) {
        self.replace = Some(Replacements {
            load: Box::new(load),
            map: OnceLock::new(),
        });
    }

    /// Stop following replace refs (commands that look at the stored
    /// objects themselves: `fsck`, pack transfer, `prune`).
    pub fn clear_replacements(&mut self) {
        self.replace = None;
    }

    /// The replace refs in use (`original -> replacement`), `None` when
    /// replacement is off.
    pub fn replacements(&self) -> Option<&HashMap<ObjectId, ObjectId>> {
        self.replace.as_ref().map(Replacements::map)
    }

    /// Whether objects are replaced at all (replacement is on and there is
    /// at least one replace ref).
    pub fn has_replacements(&self) -> bool {
        self.replacements().is_some_and(|m| !m.is_empty())
    }

    /// The object read in place of `oid`: its replacement, followed
    /// through further replacements, or `oid` itself.
    /// [`Error::ReplaceDepth`] when the chain is too long or loops.
    pub fn replaced(&self, oid: &ObjectId) -> Result<ObjectId, Error> {
        match self.replacements() {
            Some(map) if !map.is_empty() => follow_replacements(map, oid),
            _ => Ok(*oid),
        }
    }

    fn report(&self, e: &Error) {
        if let Some(sink) = &self.sink {
            sink(e);
        }
    }

    fn first_in_stores<T>(
        &self,
        report_skipped: bool,
        mut f: impl FnMut(&dyn ObjectStore, &mut dyn FnMut(&Error)) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        // Broken copies of whole stores. They are reported only when a later
        // store delivers the object; otherwise the first one is returned and
        // the caller reports it (git prints the full message then).
        let mut skipped: Vec<Error> = Vec::new();
        // Copies a store skipped internally (inside a set of packs) are
        // reported right away, as git does.
        let mut report = |e: &Error| {
            if report_skipped {
                self.report(e)
            }
        };
        for store in &self.stores {
            match f(store.as_ref(), &mut report) {
                Ok(Some(v)) => {
                    if report_skipped {
                        // A pack summary's causes were reported already.
                        for e in skipped
                            .iter()
                            .filter(|e| !matches!(e, Error::PackSummary(_)))
                        {
                            self.report(e);
                        }
                    }
                    return Ok(Some(v));
                }
                Ok(None) => {}
                Err(e) if e.is_corruption() => skipped.push(e),
                Err(e) => return Err(e),
            }
        }
        match skipped.into_iter().next() {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    /// Whether any store has the object.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.settle(oid);
        self.stores.iter().any(|s| s.contains(oid)) || is_empty_tree(oid)
    }

    /// Type and size of an object. Unlike [`ObjectDatabase::read`], copies
    /// skipped on the way are not reported (git is silent there too, e.g.
    /// `git cat-file -t` with a corrupt loose and an intact packed copy);
    /// when no copy answers, the first error is returned.
    /// Follows replace refs ([`ObjectDatabase::replaced`]).
    pub fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        self.header_unreplaced(&self.replaced(oid)?)
    }

    /// [`ObjectDatabase::header`] of the stored object, ignoring replace
    /// refs.
    pub fn header_unreplaced(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        match self.header_here(oid) {
            Ok(None) if self.fetch_missing(oid) => self.header_here(oid),
            other => other,
        }
    }

    /// [`ObjectDatabase::header_unreplaced`] without asking a promisor
    /// remote.
    pub fn header_here(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        self.settle(oid);
        match self.first_in_stores(false, |s, r| s.header_reporting(oid, r)) {
            // A delta whose base is corrupt in its pack may still be
            // readable with a base from another store.
            Err(e) if e.is_corruption() => match self.read_here(oid) {
                Ok(Some(o)) => Ok(Some(o.header())),
                _ => Err(e),
            },
            Ok(None) if is_empty_tree(oid) => Ok(Some(empty_tree().header())),
            other => other,
        }
    }

    /// An object's type and content; follows replace refs
    /// ([`ObjectDatabase::replaced`]).
    pub fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        self.read_unreplaced(&self.replaced(oid)?)
    }

    /// The type and content of the stored object, ignoring replace refs.
    pub fn read_unreplaced(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        match self.read_here(oid) {
            Ok(None) if self.fetch_missing(oid) => self.read_here(oid),
            other => other,
        }
    }

    /// Fetch `oid` from a promisor remote ([`crate::lazy`]); whether it
    /// may be here now.
    fn fetch_missing(&self, oid: &ObjectId) -> bool {
        if !crate::lazy::fetch(self.loose.objects_dir(), &[*oid]) {
            return false;
        }
        if self.contains(oid) {
            return true;
        }
        crate::lazy::mark_failed(oid);
        false
    }

    /// Fetch those of `ids` that are missing in one batch from a promisor
    /// remote ([`crate::lazy`]), as `checkout` does before writing files.
    /// Does nothing when no fetcher is installed or fetching is off.
    pub fn prefetch(&self, ids: &[ObjectId]) {
        if !crate::lazy::enabled() {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let missing: Vec<ObjectId> = ids
            .iter()
            .filter(|id| seen.insert(**id) && !self.contains(id))
            .copied()
            .collect();
        if missing.is_empty() || !crate::lazy::fetch(self.loose.objects_dir(), &missing) {
            return;
        }
        for id in missing.iter().filter(|id| !self.contains(id)) {
            crate::lazy::mark_failed(id);
        }
    }

    /// [`ObjectDatabase::read_unreplaced`] without asking a promisor
    /// remote.
    pub fn read_here(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        self.settle(oid);
        let found = self.first_in_stores(true, |s, r| {
            // A delta base that is corrupt in this store may have a good
            // copy in another one (git reads it from there).
            s.read_with_bases(oid, r, &mut |base| self.base_elsewhere(s, base))
        });
        match found {
            Ok(None) if is_empty_tree(oid) => Ok(Some(empty_tree())),
            other => other,
        }
    }

    /// Read `base` silently from the stores other than `skip`.
    fn base_elsewhere(&self, skip: &dyn ObjectStore, base: &ObjectId) -> Option<RawObject> {
        self.stores
            .iter()
            .filter(|s| !std::ptr::addr_eq(s.as_ref() as *const dyn ObjectStore, skip))
            .find_map(|s| s.read(base).ok().flatten())
    }

    /// An object, parsed.
    pub fn read_object(&self, oid: &ObjectId) -> Result<Option<Object>, ReadObjectError> {
        match self.read(oid)? {
            Some(raw) => Ok(Some(raw.parse(self.hash)?)),
            None => Ok(None),
        }
    }

    /// All objects whose id starts with `prefix`.
    pub fn resolve_prefix(&self, prefix: &Prefix) -> Result<PrefixMatch, Error> {
        self.settle_all();
        let mut found = BTreeSet::new();
        for store in &self.stores {
            store.for_each_prefix_match(prefix, &mut |oid| {
                found.insert(oid);
            })?;
        }
        Ok(match found.len() {
            0 => PrefixMatch::None,
            1 => PrefixMatch::Unique(found.into_iter().next().expect("one element")),
            _ => PrefixMatch::Ambiguous(found.into_iter().collect()),
        })
    }

    /// Whether any pack of the database is a promisor pack (has a
    /// `.promisor` file).
    pub fn has_promisor_packs(&self) -> bool {
        self.packs
            .iter()
            .chain(&self.alternate_packs)
            .any(|p| p.set().packs().iter().any(|p| p.is_promisor()))
    }

    /// The promisor objects (`Documentation/technical/partial-clone.adoc`):
    /// the objects in promisor packs and the objects they refer to (the
    /// tree and parents of a commit, the entries of a tree except
    /// submodule commits, the target of a tag). A missing object in this
    /// set is expected in a partial clone. Computed once per database.
    pub fn promisor_objects(&self) -> &HashSet<ObjectId> {
        self.promisor.get_or_init(|| {
            let mut out = HashSet::new();
            let mut cache = rgit_pack::DeltaBaseCache::default();
            for store in self.packs.iter().chain(&self.alternate_packs) {
                for pack in store.set().packs().iter().filter(|p| p.is_promisor()) {
                    for id in pack.index().ids() {
                        out.insert(id);
                        let Ok(Some(obj)) = pack.read(&id, &mut cache) else {
                            continue;
                        };
                        referenced_ids(self.hash, obj.kind, &obj.data, &mut out);
                    }
                }
            }
            out
        })
    }

    /// Whether `oid` is a promisor object ([`ObjectDatabase::promisor_objects`]).
    pub fn is_promisor_object(&self, oid: &ObjectId) -> bool {
        self.promisor_objects().contains(oid)
    }

    /// Every object id in all stores (sorted, without duplicates).
    pub fn all_objects(&self) -> Result<BTreeSet<ObjectId>, Error> {
        self.settle_all();
        let mut all = BTreeSet::new();
        for store in &self.stores {
            store.for_each_object(&mut |oid| {
                all.insert(oid);
            })?;
        }
        Ok(all)
    }

    /// Store an object unless some store has it already (then it is only
    /// freshened). Returns its id.
    pub fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, Error> {
        let oid = rgit_hash::hash_object(self.hash, kind.name(), data)?;
        if self.stores.iter().any(|s| s.freshen(&oid)) {
            return Ok(oid);
        }
        self.loose.write(kind, data)
    }

    /// Serialise and store an object.
    pub fn write_object(&self, object: &Object) -> Result<ObjectId, Error> {
        self.write(object.kind(), &object.serialize())
    }

    /// Store an object of known size from a reader (large blobs). Existing
    /// copies in other stores are only detected after the data was read.
    pub fn write_stream(
        &self,
        kind: ObjectKind,
        size: u64,
        reader: &mut dyn Read,
    ) -> Result<ObjectId, Error> {
        self.loose.write_stream(kind, size, reader)
    }
}

/// Large objects.
impl ObjectDatabase {
    /// Use `options` for large objects (the default comes from
    /// [`LargeObjectOptions::default`]; [`ObjectDatabase::open_for_repo`]
    /// reads them from the configuration).
    pub fn set_large_object_options(&mut self, options: LargeObjectOptions) {
        self.large = options;
    }

    /// The settings for large objects.
    pub fn large_object_options(&self) -> LargeObjectOptions {
        self.large
    }

    /// `core.bigFileThreshold`.
    pub fn big_file_threshold(&self) -> u64 {
        self.large.big_file_threshold
    }

    /// Whether content of `size` bytes is above `core.bigFileThreshold`.
    pub fn is_big(&self, size: u64) -> bool {
        size > self.large.big_file_threshold
    }

    /// An object's content as a stream: loose objects and whole pack
    /// entries are inflated while being read, so memory use does not
    /// depend on the object's size. Copies that cannot be opened are
    /// skipped like in [`ObjectDatabase::read`]; damage found while
    /// reading is an error of the reader.
    /// Follows replace refs ([`ObjectDatabase::replaced`]).
    pub fn stream(&self, oid: &ObjectId) -> Result<Option<ObjectReader>, Error> {
        self.stream_unreplaced(&self.replaced(oid)?)
    }

    /// [`ObjectDatabase::stream`] of the stored object, ignoring replace
    /// refs.
    pub fn stream_unreplaced(&self, oid: &ObjectId) -> Result<Option<ObjectReader>, Error> {
        self.settle(oid);
        match self.first_in_stores(true, |s, _| s.stream(oid)) {
            Ok(None) if is_empty_tree(oid) => Ok(Some(ObjectReader::from_raw(empty_tree()))),
            other => other,
        }
    }

    /// Store a large object of known `size` from `reader` in the pack of
    /// large objects of this command, unless some store has it already
    /// (then it is only freshened). The content is never held in memory.
    /// The pack becomes visible with [`ObjectDatabase::flush_bulk`], when
    /// one of its objects is looked up, or when the database is dropped.
    pub fn write_bulk(
        &self,
        kind: ObjectKind,
        size: u64,
        reader: &mut dyn Read,
    ) -> Result<ObjectId, Error> {
        let mut bulk = self.bulk.lock().unwrap_or_else(|p| p.into_inner());
        let bulk = bulk.get_or_insert_with(|| {
            Bulk::new(
                self.loose.objects_dir(),
                self.hash,
                BulkOptions {
                    compression: self.large.pack_compression,
                    size_limit: self.large.pack_size_limit,
                },
            )
        });
        let exists = |oid: &ObjectId| self.stores.iter().any(|s| s.freshen(oid));
        let oid = bulk.write(kind, size, reader, &exists)?;
        if bulk.pending(&oid) {
            self.bulk_pending.store(true, Ordering::Release);
        }
        Ok(oid)
    }

    /// Finish the pack of large objects, if one is being written: its
    /// header, checksum and index are written and it is moved into place.
    pub fn flush_bulk(&self) -> Result<(), Error> {
        let mut bulk = self.bulk.lock().unwrap_or_else(|p| p.into_inner());
        self.bulk_pending.store(false, Ordering::Release);
        match bulk.as_mut() {
            Some(b) => b.finish(),
            None => Ok(()),
        }
    }

    /// Make `oid` readable if it waits in the unfinished pack.
    fn settle(&self, oid: &ObjectId) {
        if !self.bulk_pending.load(Ordering::Acquire) {
            return;
        }
        let pending = self
            .bulk
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|b| b.pending(oid));
        if pending {
            self.flush_reporting();
        }
    }

    fn settle_all(&self) {
        if self.bulk_pending.load(Ordering::Acquire) {
            self.flush_reporting();
        }
    }

    fn flush_reporting(&self) {
        if let Err(e) = self.flush_bulk() {
            for line in e.git_lines() {
                eprintln!("error: {line}");
            }
        }
    }
}

impl Drop for ObjectDatabase {
    fn drop(&mut self) {
        self.flush_reporting();
    }
}

/// Errors of [`ObjectDatabase::read_object`].
#[derive(Debug, thiserror::Error)]
pub enum ReadObjectError {
    /// Reading failed.
    #[error(transparent)]
    Odb(#[from] Error),
    /// The object does not parse.
    #[error(transparent)]
    Parse(#[from] rgit_object::Error),
}

/// The pack writer reads objects through the database: packs, loose
/// objects and alternates. Packs hold the stored objects, never their
/// replacements.
impl rgit_pack::write::ObjectSource for ObjectDatabase {
    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, rgit_pack::write::SourceError> {
        Ok(self.read_unreplaced(id)?)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, rgit_pack::write::SourceError> {
        Ok(self.header_unreplaced(id)?.map(|h| h.kind))
    }

    fn header(
        &self,
        id: &ObjectId,
    ) -> Result<Option<(ObjectKind, u64)>, rgit_pack::write::SourceError> {
        Ok(self.header_unreplaced(id)?.map(|h| (h.kind, h.size)))
    }

    fn stream(
        &self,
        id: &ObjectId,
    ) -> Result<Option<rgit_pack::write::SourceStream<'_>>, rgit_pack::write::SourceError> {
        Ok(self.stream_unreplaced(id)?.map(|r| {
            let (kind, size) = (r.kind(), r.size());
            (kind, size, Box::new(r) as Box<dyn Read>)
        }))
    }
}

/// `index-pack` looks up objects (collision checks, thin pack bases)
/// through the database.
impl rgit_pack::index_pack::ObjectDb for ObjectDatabase {
    fn contains(&self, id: &ObjectId) -> bool {
        id.kind() == self.hash && ObjectDatabase::contains(self, id)
    }

    /// Thin pack bases missing in a partial clone are fetched from a
    /// promisor remote, even where fetching is otherwise off (`fetch`).
    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
        match ObjectDatabase::read_here(self, id) {
            Ok(None) if crate::lazy::fetch_anyway(self.loose.objects_dir(), &[*id]) => {
                ObjectDatabase::read_here(self, id).map_err(|e| e.to_string())
            }
            other => other.map_err(|e| e.to_string()),
        }
    }

    fn stream(&self, id: &ObjectId) -> Result<Option<Box<dyn Read + '_>>, String> {
        ObjectDatabase::stream_unreplaced(self, id)
            .map(|r| r.map(|r| Box::new(r) as Box<dyn Read>))
            .map_err(|e| e.to_string())
    }

    fn header(&self, id: &ObjectId) -> Result<Option<(ObjectKind, u64)>, String> {
        ObjectDatabase::header_here(self, id)
            .map(|h| h.map(|h| (h.kind, h.size)))
            .map_err(|e| e.to_string())
    }
}

/// `oid` after following its replacements in `map`.
pub fn follow_replacements(
    map: &HashMap<ObjectId, ObjectId>,
    oid: &ObjectId,
) -> Result<ObjectId, Error> {
    let mut cur = *oid;
    for _ in 0..MAX_REPLACE_DEPTH {
        match map.get(&cur) {
            Some(next) => cur = *next,
            None => return Ok(cur),
        }
    }
    Err(Error::ReplaceDepth(*oid))
}

/// git knows the empty tree without storing it: it can be read in every
/// repository (t1060 "internal tree objects are not missing").
fn is_empty_tree(oid: &ObjectId) -> bool {
    *oid == ObjectId::empty_tree(oid.kind())
}

fn empty_tree() -> RawObject {
    RawObject {
        kind: rgit_object::ObjectKind::Tree,
        data: Vec::new(),
    }
}

/// Add the ids an object refers to (see [`ObjectDatabase::promisor_objects`]).
pub fn referenced_ids(hash: HashKind, kind: ObjectKind, data: &[u8], out: &mut HashSet<ObjectId>) {
    match kind {
        ObjectKind::Commit => {
            if let Ok(c) = rgit_object::Commit::parse(hash, data) {
                if let Ok(t) = c.tree() {
                    out.insert(t);
                }
                if let Ok(ps) = c.parents() {
                    out.extend(ps);
                }
            }
        }
        ObjectKind::Tree => {
            for e in rgit_object::TreeIter::new(hash, data).flatten() {
                if e.mode_bits & 0o170000 != 0o160000 {
                    out.insert(e.oid);
                }
            }
        }
        ObjectKind::Tag => {
            if let Ok(t) = rgit_object::Tag::parse(hash, data) {
                if let Ok(o) = t.object() {
                    out.insert(o);
                }
            }
        }
        ObjectKind::Blob => {}
    }
}
