// SPDX-License-Identifier: MIT
//! Active submodules (gitsubmodules(7), "ACTIVE SUBMODULES").

use rgit_config::ConfigSet;
use rgit_pathspec::{ParseOptions, Pathspec};

/// Whether the submodule `name` at `path` is of interest to git commands:
/// `submodule.<name>.active` if set, else whether `path` matches the
/// pathspec formed by all `submodule.active` values if there are any, else
/// whether `submodule.<name>.url` is set.
pub fn is_active(config: &ConfigSet, name: &[u8], path: &[u8]) -> bool {
    let key = |k: &str| {
        let mut s = b"submodule.".to_vec();
        s.extend_from_slice(name);
        s.push(b'.');
        s.extend_from_slice(k.as_bytes());
        s
    };
    if let Some(e) = config.get(key("active")) {
        if let Ok(b) = e.bool() {
            return b;
        }
    }
    let specs: Vec<&[u8]> = config
        .get_all("submodule.active")
        .filter_map(|e| e.value.as_deref().map(|v| v.as_ref()))
        .collect();
    if !specs.is_empty() {
        let opts = ParseOptions {
            resolve_absolute: false,
            ..ParseOptions::default()
        };
        return Pathspec::parse(specs, &opts).is_ok_and(|ps| ps.matches(path));
    }
    config.get(key("url")).is_some()
}
