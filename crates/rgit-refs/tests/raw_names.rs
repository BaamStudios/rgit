// SPDX-License-Identifier: MIT
//! Reference names that are not UTF-8 (#214): the stores keep the raw bytes
//! on disk and hand out names escaped as `rgit_bstr` does.
#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;

use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, NoHooks, RefStore, RefUpdate, Target, Transaction};

const A: &str = "b2e6e911d0750ef5d57039fc461069c4a5277897";

fn oid() -> ObjectId {
    A.parse().unwrap()
}

fn names(store: &FilesStore) -> Vec<Vec<u8>> {
    store
        .iter(&IterOptions::default())
        .unwrap()
        .into_iter()
        .filter_map(|i| match i {
            IterItem::Ref(r) => Some(rgit_bstr::to_bytes(&r.name).into_owned()),
            _ => None,
        })
        .collect()
}

fn files_store(dir: &std::path::Path) -> FilesStore {
    std::fs::create_dir_all(dir.join("refs/heads")).unwrap();
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    FilesStore::new(dir, HashKind::Sha1)
}

fn create(store: &FilesStore, raw: &[u8]) {
    let name = rgit_bstr::from_bytes(raw).into_owned();
    let mut tx = Transaction::new(store);
    tx.push(RefUpdate::create(name, oid()).message("m"))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();
}

#[test]
fn loose_files_hold_raw_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = files_store(tmp.path());
    create(&store, b"refs/heads/caf\xe9");
    create(&store, b"refs/heads/caf\xc3\xa9");
    // The file name is the raw name, not the escape's UTF-8.
    let heads: Vec<Vec<u8>> = std::fs::read_dir(tmp.path().join("refs/heads"))
        .unwrap()
        .map(|e| e.unwrap().file_name().as_bytes().to_vec())
        .collect();
    assert!(heads.contains(&b"caf\xe9".to_vec()), "{heads:?}");
    // Iteration sorts by bytes: C3 A9 before E9.
    assert_eq!(
        names(&store),
        vec![
            b"refs/heads/caf\xc3\xa9".to_vec(),
            b"refs/heads/caf\xe9".to_vec()
        ]
    );
    let escaped = rgit_bstr::from_bytes(b"refs/heads/caf\xe9");
    assert_eq!(store.resolve_oid(&escaped).unwrap(), Some(oid()));
}

#[test]
fn packed_refs_hold_raw_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = files_store(tmp.path());
    create(&store, b"refs/heads/a\x80");
    create(&store, b"refs/heads/a\xc3\xa9");
    store.pack_refs(&|_: &str| true, true, &|_| None).unwrap();
    let packed = std::fs::read(tmp.path().join("packed-refs")).unwrap();
    // Sorted by bytes: 0x80 before C3 A9.
    let want = [
        format!("{A} refs/heads/a").as_bytes(),
        b"\x80\n",
        format!("{A} refs/heads/a").as_bytes(),
        b"\xc3\xa9\n",
    ]
    .concat();
    assert!(
        packed.ends_with(&want),
        "{}",
        String::from_utf8_lossy(&packed)
    );
    assert_eq!(
        names(&store),
        vec![
            b"refs/heads/a\x80".to_vec(),
            b"refs/heads/a\xc3\xa9".to_vec()
        ]
    );
}

#[test]
fn symbolic_targets_keep_raw_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let store = files_store(tmp.path());
    create(&store, b"refs/heads/caf\xe9");
    let target = rgit_bstr::from_bytes(b"refs/heads/caf\xe9").into_owned();
    let mut tx = Transaction::new(&store);
    tx.push(RefUpdate::symref_update("HEAD", target.clone(), None).no_deref(true))
        .unwrap();
    tx.commit(&mut NoHooks).unwrap();
    assert_eq!(
        std::fs::read(tmp.path().join("HEAD")).unwrap(),
        b"ref: refs/heads/caf\xe9\n"
    );
    assert_eq!(
        store.read_raw("HEAD").unwrap(),
        Some(Target::Symbolic(target))
    );
}
