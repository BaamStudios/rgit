// SPDX-License-Identifier: MIT

//! The object database: an ordered list of stores plus the loose store
//! that new objects are written to.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{Object, ObjectHeader, ObjectKind};

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

/// Reads objects from a list of stores and writes new ones as loose objects.
///
/// Lookups ask the stores in order. When a store has an object but cannot
/// deliver it intact (a corrupt loose file, say), the error is passed to the
/// error sink (see [`ObjectDatabase::set_error_sink`]; git prints such
/// errors and goes on) and the next store is tried. Only when no store
/// delivers the object is the first error returned.
pub struct ObjectDatabase {
    hash: HashKind,
    loose: Arc<LooseStore>,
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
            sink: None,
        }
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

    /// Receive errors of stores that are skipped during a lookup. The CLI
    /// prints each line of [`Error::git_lines`] as `error: <line>`.
    pub fn set_error_sink(&mut self, sink: impl Fn(&Error) + Send + Sync + 'static) {
        self.sink = Some(Box::new(sink));
    }

    fn first_in_stores<T>(
        &self,
        mut f: impl FnMut(&dyn ObjectStore) -> Result<Option<T>, Error>,
    ) -> Result<Option<T>, Error> {
        let mut first_err = None;
        for store in &self.stores {
            match f(store.as_ref()) {
                Ok(Some(v)) => return Ok(Some(v)),
                Ok(None) => {}
                Err(e) if e.is_corruption() => {
                    if let Some(sink) = &self.sink {
                        sink(&e);
                    }
                    first_err.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }

    /// Whether any store has the object.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.stores.iter().any(|s| s.contains(oid))
    }

    /// Type and size of an object.
    pub fn header(&self, oid: &ObjectId) -> Result<Option<ObjectHeader>, Error> {
        self.first_in_stores(|s| s.header(oid))
    }

    /// An object's type and content.
    pub fn read(&self, oid: &ObjectId) -> Result<Option<RawObject>, Error> {
        self.first_in_stores(|s| s.read(oid))
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
