// SPDX-License-Identifier: MIT
//! Messages and usage of `git bundle` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

pub const USAGE_CREATE: &str = "git bundle create [-q | --quiet | --progress]\n                  [--version=<version>] <file> <git-rev-list-args>";
pub const USAGE_VERIFY: &str = "git bundle verify [-q | --quiet] <file>";
pub const USAGE_LIST_HEADS: &str = "git bundle list-heads <file> [<refname>...]";
pub const USAGE_UNBUNDLE: &str = "git bundle unbundle [--progress] <file> [<refname>...]";

pub const H_CREATE_QUIET: &str = "do not show progress meter";
pub const H_PROGRESS: &str = "show progress meter";
pub const H_VERSION: &str = "specify bundle format version";
pub const H_VERIFY_QUIET: &str = "do not show bundle details";

pub const NEED_FILE: &str = "need a <file> argument";
pub const EMPTY_BUNDLE: &str = "Refusing to create empty bundle.";
pub const NEED_REPO_VERIFY: &str = "need a repository to verify a bundle";
pub const NEED_REPO_UNBUNDLE: &str = "Need a repository to unbundle.";
pub const INDEX_PACK_DIED: &str = "index-pack died";
pub const COMPLETE_HISTORY: &str = "The bundle records a complete history.";
pub const STDIN_NAME: &str = "<stdin>";
pub const UNBUNDLING: &str = "Unbundling objects";

/// `error:` for a bundle file that cannot be opened.
pub fn could_not_open(path: &str) -> String {
    format!("could not open '{path}'")
}

/// `fatal:` for `--version` other than 2 or 3.
pub fn unsupported_version(v: i32) -> String {
    format!("unsupported bundle version {v}")
}

/// `fatal:` for version 2 with SHA-256 or with a filter.
pub fn cannot_write_v2(hash: &str) -> String {
    format!("cannot write bundle version 2 with algorithm {hash}")
}

/// `warning:` for a ref whose commit the rev-list options left out.
pub fn ref_excluded(name: &str) -> String {
    format!("ref '{name}' is excluded by the rev-list options")
}

/// `<file> is okay` (on standard error) after `verify`.
pub fn is_okay(name: &str) -> String {
    format!("{name} is okay")
}

/// The heading of the ref list of `verify`.
pub fn contains_refs(n: usize) -> String {
    if n == 1 {
        "The bundle contains this ref:".to_owned()
    } else {
        format!("The bundle contains these {n} refs:")
    }
}

/// The heading of the prerequisite list of `verify`.
pub fn requires_refs(n: usize) -> String {
    if n == 1 {
        "The bundle requires this ref:".to_owned()
    } else {
        format!("The bundle requires these {n} refs:")
    }
}

/// The hash line of `verify`.
pub fn uses_hash(name: &str) -> String {
    format!("The bundle uses this hash algorithm: {name}")
}

/// The filter line of `verify`.
pub fn uses_filter(spec: &str) -> String {
    format!("The bundle uses this filter: {spec}")
}

/// `fatal:` for `bundle create` outside a repository.
pub const NEED_REPO_CREATE: &str = "Need a repository to create a bundle.";

/// `fatal:` when the bundle path is a directory.
pub fn cannot_create_dir(path: &str) -> String {
    format!("cannot create '{path}': Is a directory")
}

/// `fatal:` for a filter rgit cannot apply yet.
pub fn filter_not_supported(name: &str) -> String {
    format!("{name} filters are not supported by rgit yet")
}
