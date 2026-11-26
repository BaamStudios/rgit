// SPDX-License-Identifier: MIT

//! How the index file is read and written, from configuration
//! (`config/index.adoc`, `config/feature.adoc`): `index.version`,
//! `GIT_INDEX_VERSION`, `feature.manyFiles`, `index.skipHash`,
//! `index.threads`, `index.recordEndOfIndexEntries` and
//! `index.recordOffsetTable`.

use rgit_config::{BoolOrInt, ConfigSet};
use rgit_index::{Index, OffsetTable, ReadOptions, WriteOptions};

use crate::Die;
use crate::messages::index_files as msg;

/// The version git uses without configuration: 2, or 3 when an entry
/// needs extended flags.
const DEFAULT_VERSION: u32 = 3;

fn config_err(e: rgit_config::ValueError) -> Die {
    Die::Fatal(e.to_string())
}

fn bool_config(config: &ConfigSet, key: &str) -> Result<Option<bool>, Die> {
    config.bool(key).map_err(config_err)
}

/// `index.threads`: `None` when unset, `Some(0)` for "one per CPU"
/// (`true` or 0), `Some(1)` for no threads (`false` or 1).
fn threads(config: &ConfigSet) -> Result<Option<usize>, Die> {
    let Some(entry) = config.get("index.threads") else {
        return Ok(None);
    };
    Ok(Some(match entry.bool_or_int().map_err(config_err)? {
        BoolOrInt::Bool(true) => 0,
        BoolOrInt::Bool(false) => 1,
        BoolOrInt::Int(n) => usize::try_from(n).unwrap_or(1),
    }))
}

/// The largest `index.maxPathLen`.
const MAX_PATH_LEN_LIMIT: i64 = 2_147_483_647;

/// Applies `index.maxPathLen` to the path checks of the index; an invalid
/// value ends the command, as in git.
pub fn apply_max_path_len(config: &ConfigSet) -> Result<(), Die> {
    let Some(entry) = config.get("index.maxpathlen") else {
        rgit_index::set_max_path_len(rgit_index::DEFAULT_MAX_PATH_LEN);
        return Ok(());
    };
    let n = entry.int(rgit_config::IntWidth::U64).map_err(config_err)?;
    if n > MAX_PATH_LEN_LIMIT {
        eprintln!(
            "error: {}",
            msg::value_exceeds_maximum("index.maxpathlen", MAX_PATH_LEN_LIMIT)
        );
        return Err(Die::Fatal(crate::revisions::unable_to_parse(
            "index.maxpathlen",
            entry,
        )));
    }
    rgit_index::set_max_path_len(usize::try_from(n).unwrap_or(usize::MAX));
    Ok(())
}

/// Options for reading the index.
pub fn read_options(config: &ConfigSet) -> ReadOptions {
    ReadOptions {
        threads: threads(config).ok().flatten().unwrap_or(0),
        ..ReadOptions::default()
    }
}

/// The version of a new index: `GIT_INDEX_VERSION`, else `index.version`,
/// else 4 with `feature.manyFiles`; an invalid value warns and falls back
/// to git's default.
fn default_version(config: &ConfigSet, many_files: bool) -> Result<u32, Die> {
    if let Some(v) = std::env::var_os("GIT_INDEX_VERSION") {
        return Ok(match v.to_str().and_then(|s| s.parse::<u32>().ok()) {
            Some(n @ 2..=4) => n,
            _ => {
                eprintln!("warning: {}", msg::invalid_version_env(DEFAULT_VERSION));
                DEFAULT_VERSION
            }
        });
    }
    if let Some(v) = config.int("index.version").map_err(config_err)? {
        return Ok(match u32::try_from(v) {
            Ok(n @ 2..=4) => n,
            _ => {
                eprintln!("warning: {}", msg::invalid_version_config(DEFAULT_VERSION));
                DEFAULT_VERSION
            }
        });
    }
    Ok(if many_files { 4 } else { DEFAULT_VERSION })
}

/// Options for writing `index`. The version settings only matter for an
/// index without a version yet (a new one), and only then warn.
pub fn write_options(config: &ConfigSet, index: &Index) -> Result<WriteOptions, Die> {
    let many_files = bool_config(config, "feature.manyfiles")?.unwrap_or(false);
    let default_version = if index.version() == 0 {
        default_version(config, many_files)?
    } else {
        DEFAULT_VERSION
    };
    let threads = threads(config)?;
    // Explicitly enabled threads turn both tables on by default.
    let threaded = threads.is_some_and(|n| n != 1);
    let end_of_entries = bool_config(config, "index.recordendofindexentries")?.unwrap_or(threaded);
    let offset_table = if bool_config(config, "index.recordoffsettable")?.unwrap_or(threaded) {
        match threads.unwrap_or(0) {
            0 => OffsetTable::Auto,
            n => OffsetTable::Threads(n),
        }
    } else {
        OffsetTable::None
    };
    Ok(WriteOptions {
        skip_hash: bool_config(config, "index.skiphash")?.unwrap_or(many_files),
        default_version,
        end_of_entries,
        offset_table,
    })
}
