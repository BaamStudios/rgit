// SPDX-License-Identifier: MIT
//! `git refs migrate --ref-format=<format> [--no-reflog] [--dry-run]`
//! (Documentation/git-refs.adoc): rewrites the references and reflogs of
//! the repository in the other storage format.
//!
//! The new store is built in `<git-dir>/ref_migration.XXXXXX`. A dry run
//! stops there and prints the directory. Otherwise the files of the old
//! format are removed, the new ones moved into the git directory and
//! `extensions.refStorage` (with `core.repositoryFormatVersion`) updated.
//!
//! As observed with the reference git: a reftable store gets one table
//! with every reference at update index 1 and the reflog entries numbered
//! from 1 in the order of the logs; a files store gets `packed-refs` for
//! the direct references below `refs/` and loose files for symbolic and
//! root references. Peeled values are not carried over, logs without
//! entries neither. `FETCH_HEAD` and `MERGE_HEAD` are files in both
//! formats and stay where they are.

use std::path::{Path, PathBuf};

use rgit_config::ValuePattern;
use rgit_config::edit::SetRequest;
use rgit_refs::reflog::{Entry, FilesReflog};
use rgit_refs::{IterItem, IterOptions, RefStore, Target};
use rgit_repo::RefStorage;

use crate::messages::refs as msg;
use crate::prelude::*;
use crate::refs_support;
use crate::repo::{self, Mode};

fn format_name(f: RefStorage) -> &'static str {
    match f {
        RefStorage::Files => "files",
        RefStorage::Reftable => "reftable",
    }
}

fn failed(e: impl std::fmt::Display) -> Die {
    Die::Error(e.to_string(), 255)
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let format: RefCell<Option<String>> = RefCell::new(None);
    let dry_run = Cell::new(false);
    let reflog = Cell::new(true);
    let mut parser = Parser::new(
        &[msg::USAGE_MIGRATE],
        vec![
            Opt::string(None, "ref-format", &format, "format", msg::HELP_REF_FORMAT).noneg(),
            Opt::flag('n', "dry-run", &dry_run, msg::HELP_DRY_RUN),
            Opt::flag(None, "reflog", &reflog, msg::HELP_REFLOG),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    if !args.is_empty() {
        eprintln!("usage: {}", msg::TOO_MANY_ARGUMENTS);
        return Err(Die::Exit(rgit_options::EXIT_USAGE));
    }
    let Some(format) = format.into_inner() else {
        eprintln!("usage: {}", msg::MISSING_REF_FORMAT);
        return Err(Die::Exit(rgit_options::EXIT_USAGE));
    };
    let setup = repo::setup(ctx, Mode::Required)?;
    let target = match format.as_str() {
        "files" => RefStorage::Files,
        "reftable" => RefStorage::Reftable,
        other => return Err(failed(msg::unknown_ref_storage_format(other))),
    };
    if target == setup.format.ref_storage {
        return Err(failed(msg::already_uses_format(format_name(target))));
    }
    if crate::worktrees::list(&setup, false).len() > 1 {
        return Err(failed(msg::MIGRATE_WITH_WORKTREES));
    }
    let config = refs_support::load_config(ctx, &setup)?;
    let shared = refs_support::shared_repository(&config);
    let kind = setup.format.object_format;
    let git_dir = setup.git_dir.clone();

    // What the repository has now.
    let store = refs_support::ref_store(&setup, &config);
    let options = IterOptions {
        prefix: String::new(),
        include_root_refs: true,
    };
    let mut refs: Vec<(String, Target)> = Vec::new();
    for item in store.iter(&options).map_err(failed)? {
        // `FETCH_HEAD` and `MERGE_HEAD` are no references of the store.
        if rgit_refs::name::is_pseudo_ref(item.name()) {
            continue;
        }
        match item {
            IterItem::Ref(r) => refs.push((r.name, r.target)),
            IterItem::Broken(name) | IterItem::BadName(name) => {
                // Stored values that are no references cannot be carried
                // over; what can be read of a badly named one is.
                if let Ok(Some(t)) = store.read_raw(&name) {
                    refs.push((name, t));
                }
            }
        }
    }
    let mut logs: Vec<(String, Vec<Entry>)> = Vec::new();
    if reflog.get() {
        let old_logs = FilesReflog::new(&git_dir, &setup.common_dir, kind);
        for name in old_logs.list().map_err(failed)? {
            let entries = old_logs.read(&name).map_err(failed)?.unwrap_or_default();
            if !entries.is_empty() {
                logs.push((name, entries));
            }
        }
    }

    // The new store, next to the old one.
    let tmp = make_temp_dir(&git_dir).map_err(failed)?;
    let built = match target {
        RefStorage::Reftable => {
            rgit_refs::reftable::backend::import(&tmp, kind, shared, &refs, &logs)
        }
        RefStorage::Files => write_files(&tmp, kind, shared, &refs, &logs),
    };
    if let Err(e) = built {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(failed(e));
    }
    if dry_run.get() {
        println!("{}", msg::dry_run_finished(&tmp.display().to_string()));
        return Ok(0);
    }

    // Out with the old format, in with the new one.
    remove_old(&git_dir, setup.format.ref_storage, &refs);
    for entry in std::fs::read_dir(&tmp).map_err(failed)?.flatten() {
        let dest = git_dir.join(entry.file_name());
        if dest.is_dir() && !dest.is_symlink() {
            let _ = std::fs::remove_dir_all(&dest);
        }
        std::fs::rename(entry.path(), &dest).map_err(failed)?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    update_config(&setup.common_dir.join("config"), target)?;
    Ok(0)
}

/// Creates `<git_dir>/ref_migration.XXXXXX`.
fn make_temp_dir(git_dir: &Path) -> std::io::Result<PathBuf> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    use std::hash::{BuildHasher, Hasher};
    let mut last = None;
    for attempt in 0..100u32 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.write_u32(attempt);
        let mut seed = h.finish();
        let mut name = String::from("ref_migration.");
        for _ in 0..6 {
            name.push(ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char);
            seed /= ALPHABET.len() as u64;
        }
        let path = git_dir.join(name);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::AlreadyExists)))
}

/// Writes `refs` and `logs` into `dir` in the files format.
fn write_files(
    dir: &Path,
    kind: rgit_hash::HashKind,
    shared: rgit_fs::SharedRepository,
    refs: &[(String, Target)],
    logs: &[(String, Vec<Entry>)],
) -> std::io::Result<()> {
    for sub in ["refs", "refs/heads", "refs/tags"] {
        let path = dir.join(sub);
        std::fs::create_dir_all(&path)?;
        let _ = shared.apply(&path);
    }
    let write = |name: &str, content: &[u8]| -> std::io::Result<()> {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        let _ = shared.apply(&path);
        Ok(())
    };
    let mut packed = Vec::new();
    for (name, target) in refs {
        match target {
            Target::Object(id) if name.starts_with("refs/") => {
                packed.push(rgit_refs::packed::PackedRef {
                    name: name.clone(),
                    oid: *id,
                    peeled: None,
                });
            }
            Target::Object(id) => write(name, format!("{}\n", id.to_hex()).as_bytes())?,
            Target::Symbolic(t) => write(name, &rgit_bstr::to_bytes(&format!("ref: {t}\n")))?,
        }
    }
    let packed = rgit_refs::packed::PackedRefs::from_refs(packed);
    write("packed-refs", &packed.serialize())?;
    let new_logs = FilesReflog::new(dir, dir, kind).with_shared(shared);
    for (name, entries) in logs {
        for e in entries {
            new_logs
                .append(name, e, true)
                .map_err(std::io::Error::other)?;
        }
    }
    Ok(())
}

/// Removes what the old format keeps in the git directory.
fn remove_old(git_dir: &Path, old: RefStorage, refs: &[(String, Target)]) {
    match old {
        RefStorage::Files => {
            let _ = std::fs::remove_file(git_dir.join("packed-refs"));
            let _ = std::fs::remove_dir_all(git_dir.join("logs"));
            let _ = std::fs::remove_dir_all(git_dir.join("refs"));
            for (name, _) in refs {
                if !name.starts_with("refs/") {
                    let _ = std::fs::remove_file(git_dir.join(name));
                }
            }
        }
        RefStorage::Reftable => {
            let _ = std::fs::remove_dir_all(git_dir.join("reftable"));
            let _ = std::fs::remove_file(git_dir.join("refs").join("heads"));
        }
    }
    let _ = std::fs::remove_file(git_dir.join("HEAD"));
}

/// Records the new format in the repository configuration.
fn update_config(path: &Path, target: RefStorage) -> Result<(), Die> {
    let lock = rgit_fs::LockOptions::new();
    let set = |key: &str, value: Option<&str>| -> Result<(), Die> {
        let req = SetRequest {
            key: key.as_bytes(),
            value: value.map(str::as_bytes),
            pattern: &ValuePattern::Any,
            all: true,
            comment: None,
        };
        rgit_config::write::set_in_file(path, &lock, &req).map_err(failed)
    };
    match target {
        RefStorage::Reftable => {
            set("core.repositoryformatversion", Some("1"))?;
            set("extensions.refstorage", Some("reftable"))
        }
        RefStorage::Files => {
            set("extensions.refstorage", None)?;
            // Without an extension that needs it, the version goes back
            // to 0.
            let data = std::fs::read(path).unwrap_or_default();
            let needs_v1 = rgit_config::parse_bytes(
                &data,
                rgit_config::Origin::File(path.to_owned()),
                rgit_config::Scope::Local,
            )
            .ok()
            .and_then(|set| rgit_repo::RepositoryFormat::from_config(&set).ok())
            .is_none_or(|f| !f.v1_only.is_empty() || !f.unknown.is_empty());
            if needs_v1 {
                Ok(())
            } else {
                set("core.repositoryformatversion", Some("0"))
            }
        }
    }
}
