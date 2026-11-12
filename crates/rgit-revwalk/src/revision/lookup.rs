// SPDX-License-Identifier: MIT
//! The small interfaces the revision parser needs from a repository.
//!
//! They are deliberately narrow so that the ref store (#29), the object
//! database (#14/#207), the index and the configuration can be plugged in
//! without this crate depending on their implementations.

use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::ObjectKind;

/// A failure of a backend (corrupt object, unreadable ref, ...).
///
/// The text is printed as the reason (after `fatal: `) when it ends the
/// command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct LookupError(pub String);

/// Read access to the object database.
pub trait ObjectLookup {
    /// The hash algorithm of the repository.
    fn hash_kind(&self) -> HashKind;

    /// Type and content of an object; `Ok(None)` if it does not exist.
    fn read(&self, oid: &ObjectId) -> Result<Option<(ObjectKind, Vec<u8>)>, LookupError>;

    /// The type of an object; `Ok(None)` if it does not exist.
    fn kind(&self, oid: &ObjectId) -> Result<Option<ObjectKind>, LookupError> {
        Ok(self.read(oid)?.map(|(k, _)| k))
    }

    /// Every object id that starts with `prefix` (any order, duplicates
    /// allowed).
    fn prefix_matches(&self, prefix: &Prefix) -> Result<Vec<ObjectId>, LookupError>;

    /// An estimate of the number of objects, used for `core.abbrev=auto`.
    fn approximate_count(&self) -> u64;
}

/// The value of a reference after following symbolic references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefValue {
    /// The name of the last reference in the chain (`refs/heads/main` for a
    /// `HEAD` that points to it; the name itself for a direct reference).
    pub name: String,
    /// The object id, `None` for an unborn branch.
    pub oid: Option<ObjectId>,
}

/// One reflog entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    /// Value before the update.
    pub old: ObjectId,
    /// Value after the update.
    pub new: ObjectId,
    /// Seconds since the epoch.
    pub time: u64,
    /// Time zone offset in `hhmm` form (`+0200` is `200`).
    pub tz: i32,
    /// The message.
    pub message: String,
}

/// Read access to references.
pub trait RefLookup {
    /// Resolve the full reference `name` (`HEAD`, `FETCH_HEAD`,
    /// `refs/heads/main`), following symbolic references.
    ///
    /// `Ok(None)` if it does not exist.
    fn resolve(&self, name: &str) -> Result<Option<RefValue>, LookupError>;

    /// Call `f` for every reference whose name starts with `prefix`, in
    /// sorted order, with its (peeled through symbolic refs) value.
    fn for_each_ref(
        &self,
        prefix: &str,
        f: &mut dyn FnMut(&str, &ObjectId),
    ) -> Result<(), LookupError>;

    /// The reflog of the full reference `name`, oldest entry first.
    /// `Ok(None)` if the reference has no reflog.
    fn reflog(&self, name: &str) -> Result<Option<Vec<ReflogEntry>>, LookupError>;
}

/// Read access to the configuration (only what revisions need: branch
/// upstreams, remotes and push settings).
pub trait ConfigLookup {
    /// The last value of `key` (canonical `section.subsection.name` form with
    /// lowercase section and name).
    fn get(&self, key: &str) -> Option<String>;

    /// All values of `key`, in order.
    fn get_all(&self, key: &str) -> Vec<String>;
}

/// Read access to the index.
pub trait IndexLookup {
    /// The object id of the entry `path` (relative to the top of the work
    /// tree) at `stage` (0 to 3).
    fn entry(&self, path: &[u8], stage: u8) -> Option<ObjectId>;
}

/// The work tree as far as diagnostics need it.
pub trait WorkTreeLookup {
    /// Whether `path` (relative to the top of the work tree) exists on disk.
    fn exists(&self, path: &str) -> bool;
}

/// A configuration without any values.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoConfig;

impl ConfigLookup for NoConfig {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }

    fn get_all(&self, _key: &str) -> Vec<String> {
        Vec::new()
    }
}
