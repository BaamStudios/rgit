// SPDX-License-Identifier: MIT

//! Fetching missing objects from promisor remotes on demand (partial
//! clone, `Documentation/technical/partial-clone.adoc`).
//!
//! The object database cannot run a fetch itself. The command layer
//! installs a [`Fetcher`] with [`set_fetcher`]; when a lookup of
//! [`crate::ObjectDatabase::read`] or [`crate::ObjectDatabase::header`]
//! finds nothing, the database asks the fetcher for the object and looks
//! again. [`crate::ObjectDatabase::prefetch`] asks for many objects in one
//! batch (what `checkout` does before writing files).
//!
//! Commands that must not fetch (fetch itself, index-pack, fsck, repack,
//! `rev-list --missing=...`) turn it off with [`set_enabled`]. A fetcher
//! is never called again from inside itself, and an object it could not
//! deliver is not asked for again by the same process.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rgit_hash::ObjectId;

/// Fetches `ids` into the repository whose `objects` directory is the
/// first argument. Returns whether the fetch ran successfully (the caller
/// looks the objects up again either way).
pub type Fetcher = dyn Fn(&Path, &[ObjectId]) -> bool + Send + Sync;

static FETCHER: RwLock<Option<Arc<Fetcher>>> = RwLock::new(None);
static ENABLED: AtomicBool = AtomicBool::new(true);
static FAILED: Mutex<Option<HashSet<ObjectId>>> = Mutex::new(None);

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// Install the fetcher used for every object database of this process.
pub fn set_fetcher(f: Arc<Fetcher>) {
    *FETCHER.write().unwrap_or_else(|p| p.into_inner()) = Some(f);
}

/// Remove the fetcher.
pub fn clear_fetcher() {
    *FETCHER.write().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Turn fetching on or off; returns the previous setting.
pub fn set_enabled(on: bool) -> bool {
    ENABLED.swap(on, Ordering::SeqCst)
}

/// Whether a missing object would be fetched now.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst) && FETCHER.read().map(|f| f.is_some()).unwrap_or(false)
}

/// Run `f` with fetching turned off.
pub fn without<T>(f: impl FnOnce() -> T) -> T {
    let before = set_enabled(false);
    let out = f();
    set_enabled(before);
    out
}

/// Ask the fetcher for `ids` (those not asked for in vain before).
/// Returns whether a fetch ran.
pub(crate) fn fetch(objects_dir: &Path, ids: &[ObjectId]) -> bool {
    if !ENABLED.load(Ordering::SeqCst) {
        return false;
    }
    fetch_anyway(objects_dir, ids)
}

/// [`fetch`] also when fetching is turned off ([`set_enabled`]), as
/// `index-pack` does for the delta bases of a thin pack; not when no
/// fetcher is installed (`GIT_NO_LAZY_FETCH`).
pub(crate) fn fetch_anyway(objects_dir: &Path, ids: &[ObjectId]) -> bool {
    if ACTIVE.with(Cell::get) {
        return false;
    }
    let Some(f) = FETCHER.read().ok().and_then(|f| f.clone()) else {
        return false;
    };
    let wanted: Vec<ObjectId> = {
        let failed = FAILED.lock().unwrap_or_else(|p| p.into_inner());
        ids.iter()
            .filter(|id| failed.as_ref().is_none_or(|s| !s.contains(id)))
            .copied()
            .collect()
    };
    if wanted.is_empty() {
        return false;
    }
    ACTIVE.with(|a| a.set(true));
    let ok = f(objects_dir, &wanted);
    ACTIVE.with(|a| a.set(false));
    ok
}

/// Remember that `id` could not be fetched: it is not asked for again.
pub(crate) fn mark_failed(id: &ObjectId) {
    FAILED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_or_insert_with(HashSet::new)
        .insert(*id);
}
