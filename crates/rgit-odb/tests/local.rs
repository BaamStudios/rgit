// SPDX-License-Identifier: MIT

//! Tests that need no reference git: write/read round trips with every
//! option, and robustness against damaged files.

mod common;

use std::io::Read;
use std::path::PathBuf;

use common::Rng;
use rgit_fs::{FsyncMethod, SharedRepository};
use rgit_hash::{HashKind, Prefix};
use rgit_object::ObjectKind;
use rgit_odb::{LooseOptions, LooseStore, ObjectDatabase, PrefixMatch};

fn temp_objects(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-odb-local-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn round_trip_with_all_options() {
    let mut rng = Rng::new(5);
    for hash in HashKind::ALL {
        for fsync in [
            None,
            Some(FsyncMethod::Fsync),
            Some(FsyncMethod::WriteoutOnly),
            Some(FsyncMethod::Batch),
        ] {
            let dir = temp_objects(&format!("rt-{}-{fsync:?}", hash.name()));
            let odb = ObjectDatabase::open(
                &dir,
                hash,
                LooseOptions {
                    compression: 9,
                    fsync,
                    shared: SharedRepository::Group,
                    check_hash: true,
                },
            );
            for n in [0usize, 1, 100, 100_000] {
                let data = rng.bytes(n);
                let oid = odb.write(ObjectKind::Blob, &data).unwrap();
                assert_eq!(oid, rgit_hash::hash_object(hash, "blob", &data).unwrap());
                let back = odb.read(&oid).unwrap().unwrap();
                assert_eq!(back.kind, ObjectKind::Blob);
                assert_eq!(back.data, data);
                assert_eq!(odb.header(&oid).unwrap().unwrap().size, n as u64);
                assert!(odb.contains(&oid));
                let streamed = odb
                    .write_stream(ObjectKind::Blob, data.len() as u64, &mut &data[..])
                    .unwrap();
                assert_eq!(streamed, oid);
                assert_eq!(
                    odb.resolve_prefix(&Prefix::new(oid, hash.hex_len()).unwrap())
                        .unwrap(),
                    PrefixMatch::Unique(oid)
                );
            }
            // No temp files are left behind.
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("tmp_obj_"))
                .collect();
            assert!(leftovers.is_empty());
            let missing = rgit_hash::hash_object(hash, "blob", b"never written").unwrap();
            assert!(odb.read(&missing).unwrap().is_none());
            assert!(odb.header(&missing).unwrap().is_none());
            // Like git, the empty tree is there without being stored.
            let empty_tree = hash.empty_tree();
            assert_eq!(odb.read(&empty_tree).unwrap().unwrap().data, b"");
            assert!(odb.contains(&empty_tree));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn short_stream_fails_and_cleans_up() {
    let dir = temp_objects("short");
    let store = LooseStore::new(&dir, HashKind::Sha1, LooseOptions::default());
    let r = store.write_stream(ObjectKind::Blob, 10, &mut &b"abc"[..]);
    assert!(r.is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn damaged_files_never_panic() {
    let dir = temp_objects("damaged");
    let store = LooseStore::new(&dir, HashKind::Sha1, LooseOptions::default());
    let mut rng = Rng::new(77);
    let data: Vec<u8> = rng.bytes(3000);
    let oid = store.write(ObjectKind::Blob, &data).unwrap();
    let path = store.path_of(&oid);
    let good = std::fs::read(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let mut errors = 0;
    for i in 0..3000 {
        let mut d = good.clone();
        match i % 3 {
            0 => {
                let k = (rng.next() as usize) % d.len();
                d[k] ^= 1 << (rng.next() % 8);
            }
            1 => d.truncate((rng.next() as usize) % d.len()),
            _ => {
                let n = 1 + (rng.next() % 8) as usize;
                d.extend_from_slice(&rng.bytes(n));
            }
        }
        std::fs::write(&path, &d).unwrap();
        let r = store.read(&oid);
        let _ = store.header(&oid);
        let v = store.verify(&oid);
        if let Ok(Some(mut reader)) = store.open(&oid) {
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink);
        }
        if r.is_err() || v.is_err() {
            errors += 1;
        }
        if let Ok(obj) = v {
            assert_eq!(obj.data, data, "verify accepted wrong content");
        }
    }
    assert!(errors > 2000);
    std::fs::remove_dir_all(dir).unwrap();
}
