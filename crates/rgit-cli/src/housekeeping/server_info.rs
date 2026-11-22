// SPDX-License-Identifier: MIT
//! The catalog files for dumb transports (`git-update-server-info(1)`,
//! `gitrepository-layout(5)`): `info/refs` lists every reference as
//! `<id> TAB <name>`, followed by `<peeled id> TAB <name>^{}` for
//! annotated tags; `objects/info/packs` lists the local packs as
//! `P <file name>` and ends with an empty line.

use std::io::Write;
use std::path::Path;

use rgit_object::{ObjectKind, Tag};
use rgit_odb::ObjectDatabase;
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore};
use rgit_repo::Setup;

use super::store::LocalPack;

/// Rewrite `info/refs` and `objects/info/packs`.
pub fn update(setup: &Setup, odb: &ObjectDatabase, packs: &[LocalPack]) -> std::io::Result<()> {
    let kind = setup.format.object_format;
    let common = setup.absolute_common_dir();
    let store = FilesStore::new(&common, kind).with_worktree(setup.absolute_git_dir());
    let mut refs = Vec::new();
    for item in store
        .iter(&IterOptions::default())
        .map_err(std::io::Error::other)?
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
    write_atomic(&common.join("info").join("refs"), &refs)?;

    let mut names: Vec<&str> = packs.iter().map(|p| p.file_name.as_str()).collect();
    names.sort_unstable();
    let mut list = Vec::new();
    for n in names {
        list.extend_from_slice(format!("P {n}\n").as_bytes());
    }
    list.push(b'\n');
    let objects = rgit_odb::objects_dir(&setup.common_dir);
    write_atomic(&objects.join("info").join("packs"), &list)
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = rgit_fs::TempFile::with_mode(dir, "tmp_info_", 0o644)?;
    tmp.file_mut().write_all(data)?;
    tmp.persist(path)
}
