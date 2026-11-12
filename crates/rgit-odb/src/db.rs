// SPDX-License-Identifier: MIT

//! The object database: an ordered list of stores plus the loose store
//! that new objects are written to.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{Object, ObjectHeader, ObjectKind};

use rgit_config::ConfigSet;

use crate::alternates;
use crate::packed::PackStore;
use crate::store::{ObjectStore, RawObject};
use crate::{Error, LooseOptions, LooseStore};

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
pub struct ObjectDatabase {
    hash: HashKind,
    loose: Arc<LooseStore>,
    packs: Option<Arc<PackStore>>,
    stores: Vec<Arc<dyn ObjectStore>>,
    sink: Option<ErrorSink>,
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
            db.push_store(Arc::new(PackStore::open(alt.join("pack"), hash)));
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
        let hash = setup.format.object_format;
        let db = ObjectDatabase::open_with_packs(objects_dir(&setup.common_dir), hash, options);
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
        self.stores.iter().any(|s| s.contains(oid)) || is_empty_tree(oid)
    }

    /// Type and size of an object. Unlike [`ObjectDatabase::read`], copies
    /// skipped on the way are not reported (git is silent there too, e.g.
    /// `git cat-file -t` with a corrupt loose and an intact packed copy);
    /// when no copy answers, the first error is returned.
    pub fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        match self.first_in_stores(false, |s, r| s.header_reporting(oid, r)) {
            // A delta whose base is corrupt in its pack may still be
            // readable with a base from another store.
            Err(e) if e.is_corruption() => match self.read(oid) {
                Ok(Some(o)) => Ok(Some(o.header())),
                _ => Err(e),
            },
            Ok(None) if is_empty_tree(oid) => Ok(Some(empty_tree().header())),
            other => other,
        }
    }

    /// An object's type and content.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
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

    /// Every object id in all stores (sorted, without duplicates).
    pub fn all_objects(&self) -> Result<BTreeSet<ObjectId>, Error> {
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
/// objects and alternates.
impl rgit_pack::write::ObjectSource for ObjectDatabase {
    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, rgit_pack::write::SourceError> {
        Ok(ObjectDatabase::read(self, id)?)
    }

    fn kind(&self, id: &ObjectId) -> Result<Option<ObjectKind>, rgit_pack::write::SourceError> {
        Ok(self.header(id)?.map(|h| h.kind))
    }
}

/// `index-pack` looks up objects (collision checks, thin pack bases)
/// through the database.
impl rgit_pack::index_pack::ObjectDb for ObjectDatabase {
    fn contains(&self, id: &ObjectId) -> bool {
        id.kind() == self.hash && ObjectDatabase::contains(self, id)
    }

    fn read(&self, id: &ObjectId) -> Result<Option<RawObject>, String> {
        ObjectDatabase::read(self, id).map_err(|e| e.to_string())
    }
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
