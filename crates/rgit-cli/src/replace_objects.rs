// SPDX-License-Identifier: MIT
//! Object replacement (`git-replace(1)`): which replace refs are in use and
//! how they reach the object database.
//!
//! Replace refs live below `refs/replace/` (`$GIT_REPLACE_REF_BASE`), one
//! per replaced object, named after its id. Every command reads objects
//! through them unless `--no-replace-objects`, `$GIT_NO_REPLACE_OBJECTS`
//! or `core.useReplaceRefs=false` turns that off. Commands that look at
//! the stored objects themselves (fsck, pack transfer, prune, commit-graph
//! writing) never use them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_odb::ObjectDatabase;
use rgit_repo::Setup;

/// `--no-replace-objects` was given.
static DISABLED: AtomicBool = AtomicBool::new(false);

/// The default namespace of replace refs.
pub const DEFAULT_BASE: &str = "refs/replace/";

/// Turn replacement off for this process (`git --no-replace-objects`).
pub fn disable() {
    DISABLED.store(true, Ordering::Relaxed);
}

/// Whether commands follow replace refs: not with `--no-replace-objects`,
/// `$GIT_NO_REPLACE_OBJECTS` (any value) or `core.useReplaceRefs=false`.
pub fn enabled(config: &ConfigSet) -> bool {
    !DISABLED.load(Ordering::Relaxed)
        && std::env::var_os("GIT_NO_REPLACE_OBJECTS").is_none()
        && config.bool("core.useReplaceRefs").ok().flatten() != Some(false)
}

/// The namespace of replace refs: `$GIT_REPLACE_REF_BASE` if set and not
/// empty, else `refs/replace/`.
pub fn ref_base() -> String {
    std::env::var("GIT_REPLACE_REF_BASE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_owned())
}

/// The replace refs of the repository of `setup` as
/// `original -> replacement` (refs whose name is not an object id are
/// ignored).
pub fn load(setup: &Setup) -> HashMap<ObjectId, ObjectId> {
    let base = ref_base();
    let hash = setup.format.object_format;
    let mut map = HashMap::new();
    for (name, target) in crate::refs::Refs::new(setup).list(&base) {
        let hex = &name[base.len().min(name.len())..];
        if let Ok(from) = ObjectId::from_hex(hash, hex.as_bytes()) {
            map.insert(from, target);
        }
    }
    map
}

/// Let `odb` read objects through the replace refs of `setup`, if
/// replacement is enabled. The refs are read when the first object is.
pub fn install(odb: &mut ObjectDatabase, setup: &Setup, config: &ConfigSet) {
    if !enabled(config) {
        odb.clear_replacements();
        return;
    }
    let setup = setup.clone();
    odb.set_replacements(move || load(&setup));
}
