// SPDX-License-Identifier: MIT
//! The catalog files for dumb transports (`git-update-server-info(1)`,
//! `gitrepository-layout(5)`): `info/refs` lists every reference as
//! `<id> TAB <name>`, followed by `<peeled id> TAB <name>^{}` for
//! annotated tags; `objects/info/packs` lists the local packs as
//! `P <file name>` and ends with an empty line.

use std::io::Write;
use std::path::{Path, PathBuf};

use rgit_object::{ObjectKind, Tag};
use rgit_odb::ObjectDatabase;
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;

use super::store::LocalPack;

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

    let mut names: Vec<&str> = packs.iter().map(|p| p.file_name.as_str()).collect();
    names.sort_unstable();
    let mut list = Vec::new();
    for n in names {
        list.extend_from_slice(format!("P {n}\n").as_bytes());
    }
    list.push(b'\n');
    let objects = rgit_odb::objects_dir(&setup.common_dir);
    let packs_path = objects.join("info").join("packs");
    let listed = |data: &[u8]| {
        let mut lines: Vec<Vec<u8>> = data
            .split(|&b| b == b'\n')
            .filter(|l| !l.is_empty())
            .map(<[u8]>::to_vec)
            .collect();
        lines.sort_unstable();
        lines
    };
    let unchanged = std::fs::read(&packs_path)
        .ok()
        .is_some_and(|old| listed(&old) == listed(&list));
    if force || !unchanged {
        write_atomic(&packs_path, &list, shared).map_err(|e| (packs_path, e))?;
    }
    Ok(())
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
