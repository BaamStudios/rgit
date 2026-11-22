// SPDX-License-Identifier: MIT
//! Effective `submodule.<name>.*` settings: the repository configuration
//! (`$GIT_DIR/config` and the other configuration files) overrides the
//! values from `.gitmodules`.

use bstr::BString;
use rgit_config::ConfigSet;

use crate::config::{
    FetchRecurse, Ignore, Submodule, Update, parse_fetch_recurse, parse_ignore, parse_update,
};

fn key(name: &[u8], k: &str) -> Vec<u8> {
    let mut s = b"submodule.".to_vec();
    s.extend_from_slice(name);
    s.push(b'.');
    s.extend_from_slice(k.as_bytes());
    s
}

/// `submodule.<name>.ignore`: configuration, then `.gitmodules`. Invalid
/// configuration values are skipped.
pub fn ignore(config: &ConfigSet, name: &[u8], gitmodules: Option<&Submodule>) -> Option<Ignore> {
    config
        .get(key(name, "ignore"))
        .and_then(|e| e.value.as_deref().and_then(|v| parse_ignore(v)))
        .or_else(|| gitmodules.and_then(|s| s.ignore))
}

/// `submodule.<name>.url` from the configuration only (set by
/// `git submodule init`).
pub fn url(config: &ConfigSet, name: &[u8]) -> Option<BString> {
    config.get(key(name, "url")).and_then(|e| e.value.clone())
}

/// `submodule.<name>.update`: configuration (where `!command` is allowed),
/// then `.gitmodules`.
pub fn update(config: &ConfigSet, name: &[u8], gitmodules: Option<&Submodule>) -> Option<Update> {
    config
        .get(key(name, "update"))
        .and_then(|e| e.value.as_deref().and_then(|v| parse_update(v, true)))
        .or_else(|| gitmodules.and_then(|s| s.update.clone()))
}

/// `submodule.<name>.branch`: configuration, then `.gitmodules`.
pub fn branch(config: &ConfigSet, name: &[u8], gitmodules: Option<&Submodule>) -> Option<BString> {
    config
        .get(key(name, "branch"))
        .and_then(|e| e.value.clone())
        .or_else(|| gitmodules.and_then(|s| s.branch.clone()))
}

/// `submodule.<name>.fetchRecurseSubmodules`: configuration, then
/// `.gitmodules`.
pub fn fetch_recurse(
    config: &ConfigSet,
    name: &[u8],
    gitmodules: Option<&Submodule>,
) -> Option<FetchRecurse> {
    config
        .get(key(name, "fetchRecurseSubmodules"))
        .and_then(|e| parse_fetch_recurse(e.value.as_deref().map(|v| v.as_ref())))
        .or_else(|| gitmodules.and_then(|s| s.fetch_recurse))
}

/// `submodule.recurse` (default false).
pub fn recurse(config: &ConfigSet) -> bool {
    config
        .bool("submodule.recurse")
        .ok()
        .flatten()
        .unwrap_or(false)
}
