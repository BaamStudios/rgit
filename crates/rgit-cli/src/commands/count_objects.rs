// SPDX-License-Identifier: MIT
//! `git count-objects` (Documentation/git-count-objects.adoc): count loose
//! objects and their disk usage; with `-v` also packs, objects that are
//! both loose and packed, garbage files and alternates.
//!
//! Loose sizes are disk usage (allocated blocks), pack and garbage sizes
//! are file sizes, as git reports them. Files in `objects/pack/` are
//! grouped by base name: a group with both `.idx` and `.pack` is a pack,
//! other groups and unknown names are garbage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rgit_hash::{HashKind, ObjectId};
use rgit_pack::PackIndex;

use crate::messages::verify_pack as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};
use crate::repo_objects::Repo;

/// Extensions that belong to a pack.
const PACK_EXTS: &[&str] = &["idx", "pack", "keep", "rev", "bitmap", "mtimes", "promisor"];

#[derive(Default)]
struct Garbage {
    count: u64,
    size: u64,
}

impl Garbage {
    fn add(&mut self, path: &Path, warning: &str) {
        self.count += 1;
        self.size += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        eprintln!("warning: {warning}");
    }
}

fn disk_usage(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

fn sorted_entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

#[derive(Default)]
struct PackScan {
    in_pack: u64,
    packs: u64,
    size_pack: u64,
    indexes: Vec<PackIndex>,
}

/// Look at `objects/pack/`: count the packs and report garbage. git does
/// this when it first needs the packs, so the warnings come at that point.
fn scan_packs(objects: &Path, hash: HashKind, garbage: &mut Garbage) -> PackScan {
    let mut scan = PackScan::default();
    let pack_dir = objects.join("pack");
    let mut groups: BTreeMap<String, Vec<(String, PathBuf)>> = BTreeMap::new();
    let mut late = Vec::new();
    let mut index_errors = Vec::new();
    for (name, path) in sorted_entries(&pack_dir) {
        if name.starts_with("multi-pack-index") {
            continue;
        }
        match name.rsplit_once('.') {
            Some((base, ext)) if PACK_EXTS.contains(&ext) => {
                groups.entry(base.to_string()).or_default().push((ext.to_string(), path));
            }
            _ => garbage.add(&path, &msg::garbage(&path.display().to_string())),
        }
    }
    for files in groups.values() {
        let find = |e: &str| files.iter().find(|(x, _)| x == e).map(|(_, p)| p);
        let (idx, pack) = (find("idx"), find("pack"));
        if let (Some(idx_path), Some(pack_path)) = (idx, pack) {
            match PackIndex::open(idx_path, hash) {
                Ok(index) => {
                    scan.packs += 1;
                    scan.in_pack += u64::from(index.len());
                    scan.size_pack += std::fs::metadata(idx_path).map(|m| m.len()).unwrap_or(0)
                        + std::fs::metadata(pack_path).map(|m| m.len()).unwrap_or(0);
                    scan.indexes.push(index);
                }
                Err(e) => index_errors.push(e.to_string()),
            }
            continue;
        }
        let what = if idx.is_some() {
            msg::NO_PACK
        } else if pack.is_some() {
            msg::NO_IDX
        } else {
            msg::NO_IDX_OR_PACK
        };
        for (_, path) in files {
            late.push((path.clone(), format!("{what}: {}", path.display())));
        }
    }
    for (path, w) in late {
        garbage.add(&path, &w);
    }
    for e in index_errors {
        eprintln!("error: {e}");
    }
    scan
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let verbose = Cell::new(false);
    let human = Cell::new(false);
    let mut parser = Parser::new(
        msg::COUNT_OBJECTS_USAGE,
        vec![
            Opt::flag('v', "verbose", &verbose, msg::HELP_BE_VERBOSE),
            Opt::flag('H', "human-readable", &human, msg::HELP_HUMAN),
        ],
    );
    let args = parser.parse(argv)?.args;
    if !args.is_empty() {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let repo = Repo::open(ctx, setup)?;
    let objects = crate::repo_objects::objects_dir(&repo.setup);
    let hash = repo.hash;
    let hex_len = hash.hex_len();
    let mut garbage = Garbage::default();

    // Loose objects.
    let mut loose = 0u64;
    let mut loose_size = 0u64;
    let mut packable = 0u64;
    let mut scan: Option<PackScan> = None;
    let is_hex = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    for (dir_name, dir) in sorted_entries(&objects) {
        if dir_name.len() != 2 || !is_hex(&dir_name) || !dir.is_dir() {
            continue;
        }
        for (name, path) in sorted_entries(&dir) {
            let id = (name.len() == hex_len - 2 && is_hex(&name))
                .then(|| ObjectId::from_hex(hash, format!("{dir_name}{name}").as_bytes()).ok())
                .flatten();
            match id {
                Some(id) => {
                    let Ok(meta) = std::fs::symlink_metadata(&path) else {
                        continue;
                    };
                    loose += 1;
                    loose_size += disk_usage(&meta);
                    if verbose.get() {
                        if scan.is_none() {
                            scan = Some(scan_packs(&objects, hash, &mut garbage));
                        }
                        let s = scan.as_ref().expect("scanned");
                        if s.indexes.iter().any(|i| i.lookup(&id).is_some()) {
                            packable += 1;
                        }
                    }
                }
                None => {
                    if verbose.get() {
                        garbage.add(&path, &msg::garbage(&path.display().to_string()));
                    }
                }
            }
        }
    }

    let scan = match scan {
        Some(s) => s,
        None if verbose.get() => scan_packs(&objects, hash, &mut garbage),
        None => PackScan::default(),
    };
    let size = |n: u64| {
        if human.get() {
            rgit_pack::index_pack::messages::humanise_bytes(n)
        } else {
            (n / 1024).to_string()
        }
    };
    let mut out = String::new();
    if verbose.get() {
        out.push_str(&format!("count: {loose}\n"));
        out.push_str(&format!("size: {}\n", size(loose_size)));
        out.push_str(&format!("in-pack: {}\n", scan.in_pack));
        out.push_str(&format!("packs: {}\n", scan.packs));
        out.push_str(&format!("size-pack: {}\n", size(scan.size_pack)));
        out.push_str(&format!("prune-packable: {packable}\n"));
        out.push_str(&format!("garbage: {}\n", garbage.count));
        out.push_str(&format!("size-garbage: {}\n", size(garbage.size)));
        for alt in repo.alternate_dirs() {
            let abs = rgit_fs::path::canonicalize(&alt).unwrap_or(alt);
            let mut q = Vec::new();
            crate::quote::write_quoted(&mut q, abs.as_os_str().as_encoded_bytes(), true);
            out.push_str(&format!("alternate: {}\n", String::from_utf8_lossy(&q)));
        }
    } else {
        out.push_str(&msg::count_summary(loose, &size(loose_size), human.get()));
        out.push('\n');
    }
    print!("{out}");
    Ok(0)
}
