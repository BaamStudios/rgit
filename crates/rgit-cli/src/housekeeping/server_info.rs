// SPDX-License-Identifier: MIT
//! The catalog files for dumb transports (`git-update-server-info(1)`,
//! `gitrepository-layout(5)`): `info/refs` lists every reference as
//! `<id> TAB <name>`, followed by `<peeled id> TAB <name>^{}` for
//! annotated tags; `objects/info/packs` lists the local packs as
//! `P <file name>` and ends with an empty line.
//!
//! The order of `objects/info/packs` follows git (observed on the git
//! binary, it is not documented): packs outside the multi-pack index in
//! directory order (as `readdir` returns the `.idx` files), then the packs
//! of the multi-pack index in its order. Without `-f` the packs already
//! listed keep their place and new ones are appended; a listed pack that
//! no longer exists, or a line of the old `D`/`T` formats, makes the list
//! start over.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_object::{ObjectKind, Tag};
use rgit_odb::ObjectDatabase;
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;

use super::store::LocalPack;
use crate::messages::housekeeping as msg;

/// Rewrite `info/refs` and `objects/info/packs`. Unless `force`, a file
/// whose content would not change is left alone (its mtime stays, as
/// t5200 checks); for `objects/info/packs` that means the same set of
/// packs. A failure names the file (as `setup` spells the repository path)
/// that could not be written.
pub fn update(
    setup: &Setup,
    odb: &ObjectDatabase,
    packs: &[LocalPack],
    shared: rgit_fs::SharedRepository,
    force: bool,
) -> Result<(), (PathBuf, std::io::Error)> {
    let kind = setup.format.object_format;
    let common = setup.absolute_common_dir();
    let store = FilesStore::new(&common, kind).with_worktree(setup.absolute_git_dir());
    let mut refs = Vec::new();
    let refs_path = setup.common_dir.join("info").join("refs");
    for item in store
        .iter(&IterOptions::default())
        .map_err(|e| (refs_path.clone(), std::io::Error::other(e)))?
    {
        let IterItem::Ref(r) = item else { continue };
        let Some(id) = r.oid else { continue };
        refs.extend_from_slice(format!("{}\t{}\n", id.to_hex(), r.name).as_bytes());
        let mut peeled = id;
        let mut was_tag = false;
        for _ in 0..64 {
            match odb.read(&peeled) {
                Ok(Some(o)) if o.kind == ObjectKind::Tag => {
                    match Tag::parse(kind, &o.data).and_then(|t| t.object()) {
                        Ok(t) => {
                            peeled = t;
                            was_tag = true;
                        }
                        Err(_) => break,
                    }
                }
                _ => break,
            }
        }
        if was_tag {
            refs.extend_from_slice(format!("{}\t{}^{{}}\n", peeled.to_hex(), r.name).as_bytes());
        }
    }
    let refs_file = common.join("info").join("refs");
    if force || std::fs::read(&refs_file).ok().as_deref() != Some(&refs[..]) {
        write_atomic(&refs_file, &refs, shared).map_err(|e| (refs_path, e))?;
    }

    let objects = rgit_odb::objects_dir(&setup.common_dir);
    let packs_path = objects.join("info").join("packs");
    let order = pack_order(odb, packs);
    let old = std::fs::read(&packs_path).ok();
    let names = match old.as_deref().filter(|_| !force) {
        Some(old) => keep_listed_order(old, order),
        None => order,
    };
    let mut list = Vec::new();
    for n in names {
        list.extend_from_slice(format!("P {n}\n").as_bytes());
    }
    list.push(b'\n');
    let unchanged = old.as_deref() == Some(&list[..]);
    if force || !unchanged {
        write_atomic(&packs_path, &list, shared).map_err(|e| (packs_path, e))?;
    }
    Ok(())
}

/// The local packs (`pack-<hash>.pack` names) in git's order: those the
/// multi-pack index does not cover in directory order, then the covered
/// ones in the order of the index.
fn pack_order(odb: &ObjectDatabase, packs: &[LocalPack]) -> Vec<String> {
    let known: HashSet<&str> = packs.iter().map(|p| p.file_name.as_str()).collect();
    let mut midx_names: Vec<String> = Vec::new();
    if let Some(store) = odb.packs()
        && let Some(chain) = store.set().midx()
    {
        for layer in chain.layers() {
            for name in layer.pack_names() {
                let stem = name.strip_suffix(".idx").unwrap_or(name);
                let pack = format!("{stem}.pack");
                if known.contains(pack.as_str()) && !midx_names.contains(&pack) {
                    midx_names.push(pack);
                }
            }
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(packs.len());
    let dir = packs
        .first()
        .and_then(|p| p.pack.path().parent().map(Path::to_path_buf));
    if let Some(dir) = dir {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".idx")) else {
                continue;
            };
            let pack = format!("{stem}.pack");
            if known.contains(pack.as_str()) && !midx_names.contains(&pack) && !out.contains(&pack)
            {
                out.push(pack);
            }
        }
    }
    // Packs the directory scan missed (a concurrent repack).
    for p in packs {
        if !out.contains(&p.file_name) && !midx_names.contains(&p.file_name) {
            out.push(p.file_name.clone());
        }
    }
    out.extend(midx_names);
    out
}

/// Without `-f`: the packs listed in the old file `old`, in its order,
/// then the other packs of `order`. A listed pack that is
/// gone, or a `D`/`T` line of the old formats, discards the old list;
/// other lines are reported and skipped.
fn keep_listed_order(old: &[u8], order: Vec<String>) -> Vec<String> {
    // A pack listed twice takes the place of its last line.
    let mut kept: Vec<String> = Vec::new();
    for line in old.split(|&b| b == b'\n') {
        match line {
            [] => {}
            [b'P', b' ', name @ ..] if !name.is_empty() => {
                let name = String::from_utf8_lossy(name);
                if !order.iter().any(|n| *n == name) {
                    return order;
                }
                kept.retain(|n| *n != name);
                kept.push(name.into_owned());
            }
            [b'D' | b'T', ..] => return order,
            _ => {
                let _ = writeln!(
                    std::io::stderr(),
                    "error: {}",
                    msg::info_packs_unrecognized(&String::from_utf8_lossy(line))
                );
            }
        }
    }
    for n in order {
        if !kept.contains(&n) {
            kept.push(n);
        }
    }
    kept
}

fn write_atomic(
    path: &Path,
    data: &[u8],
    shared: rgit_fs::SharedRepository,
) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
        let _ = shared.apply(dir);
    }
    let mut tmp = rgit_fs::TempFile::with_mode(dir, "tmp_info_", 0o666)?;
    tmp.file_mut().write_all(data)?;
    super::store::share(tmp.path(), shared);
    tmp.persist(path)
}
