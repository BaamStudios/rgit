// SPDX-License-Identifier: MIT

//! Loose objects: rgit and the reference git read each other's objects at
//! every compression level, prefix lookup matches `git rev-parse
//! --disambiguate`, permissions match under `core.sharedRepository`, and
//! corruption is reported with git's messages.

mod common;

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{Git, Rng, reference, repo};
use rgit_config::{Origin, Scope, parse_bytes};
use rgit_fs::SharedRepository;
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{ObjectHeader, ObjectKind};
use rgit_odb::{Error, LooseOptions, LooseStore, ObjectDatabase, PrefixMatch};

fn opts(level: u32) -> LooseOptions {
    LooseOptions {
        compression: level,
        ..LooseOptions::default()
    }
}

fn sample_blobs(rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut v = vec![
        Vec::new(),
        b"hello\n".to_vec(),
        b"\0\x01\x02 binary \xff".to_vec(),
        "line\n".repeat(10_000).into_bytes(),
    ];
    for n in [1, 31, 32, 33, 4096, 70_000, 1 << 20] {
        v.push(rng.bytes(n));
    }
    v
}

#[test]
fn rgit_reads_objects_written_by_git() {
    let Some(bin) = reference() else { return };
    let mut rng = Rng::new(1);
    for hash in HashKind::ALL {
        for level in ["0", "1", "9", "-1"] {
            let g = repo(&bin, "gitwrites", hash);
            g.run(&["config", "core.looseCompression", level], b"");
            for blob in sample_blobs(&mut rng) {
                g.run(&["hash-object", "-w", "--stdin"], &blob);
            }
            std::fs::write(g.dir.join("f"), "content\n").unwrap();
            g.run(&["add", "f"], b"");
            g.run(&["commit", "-q", "-m", "c1"], b"");
            g.run(&["tag", "-a", "-m", "tag msg", "v1"], b"");

            let store = LooseStore::new(g.objects(), hash, LooseOptions::default());
            let mut ids = Vec::new();
            store.for_each_object(&mut |oid| ids.push(oid)).unwrap();
            ids.sort();
            let listed = g.run(
                &[
                    "cat-file",
                    "--batch-all-objects",
                    "--batch-check=%(objectname)",
                ],
                b"",
            );
            let theirs: Vec<ObjectId> = String::from_utf8(listed)
                .unwrap()
                .lines()
                .map(|l| l.parse().unwrap())
                .collect();
            assert_eq!(ids, theirs, "level {level}");
            for oid in ids {
                let hex = oid.to_hex();
                let kind = String::from_utf8(g.run(&["cat-file", "-t", &hex], b"")).unwrap();
                let kind: ObjectKind = kind.trim().parse().unwrap();
                let content = g.run(&["cat-file", kind.name(), &hex], b"");
                let h = store.header(&oid).unwrap().unwrap();
                assert_eq!(
                    h,
                    ObjectHeader {
                        kind,
                        size: content.len() as u64
                    }
                );
                let raw = store.read(&oid).unwrap().unwrap();
                assert_eq!(raw.kind, kind);
                assert!(raw.data == content, "{oid} level {level}");
                let verified = store.verify(&oid).unwrap();
                assert_eq!(verified, raw);
                let mut streamed = Vec::new();
                store
                    .open(&oid)
                    .unwrap()
                    .unwrap()
                    .read_to_end(&mut streamed)
                    .unwrap();
                assert!(streamed == content);
            }
        }
    }
}

#[test]
fn git_reads_objects_written_by_rgit() {
    let Some(bin) = reference() else { return };
    let mut rng = Rng::new(2);
    for hash in HashKind::ALL {
        for level in [0, 1, 6, 9] {
            let g = repo(&bin, "rgitwrites", hash);
            let odb = ObjectDatabase::open(g.objects(), hash, opts(level));
            let mut written = Vec::new();
            for blob in sample_blobs(&mut rng) {
                let oid = odb.write(ObjectKind::Blob, &blob).unwrap();
                assert_eq!(oid, g.oid(&["hash-object", "--stdin"], &blob));
                assert!(odb.loose().path_of(&oid).is_file());
                written.push((oid, ObjectKind::Blob, blob));
            }
            let big = rng.bytes(300_000);
            let oid = odb
                .write_stream(ObjectKind::Blob, big.len() as u64, &mut &big[..])
                .unwrap();
            assert_eq!(oid, g.oid(&["hash-object", "--stdin"], &big));
            written.push((oid, ObjectKind::Blob, big));
            // A tree, a commit and a tag on top of the blobs.
            let blob = written[1].0;
            let mut tree = b"100644 hello\0".to_vec();
            tree.extend_from_slice(blob.as_bytes());
            let tree_id = odb.write(ObjectKind::Tree, &tree).unwrap();
            written.push((tree_id, ObjectKind::Tree, tree));
            let commit = format!(
                "tree {tree_id}\nauthor A <a@x> 1 +0000\ncommitter C <c@x> 1 +0000\n\nmsg\n"
            )
            .into_bytes();
            let commit_id = odb.write(ObjectKind::Commit, &commit).unwrap();
            written.push((commit_id, ObjectKind::Commit, commit));
            let tag =
                format!("object {commit_id}\ntype commit\ntag v1\ntagger T <t@x> 1 +0000\n\nm\n")
                    .into_bytes();
            let tag_id = odb.write(ObjectKind::Tag, &tag).unwrap();
            written.push((tag_id, ObjectKind::Tag, tag));
            g.run(&["update-ref", "refs/tags/v1", &tag_id.to_hex()], b"");

            for (oid, kind, data) in &written {
                let hex = oid.to_hex();
                let t = g.run(&["cat-file", "-t", &hex], b"");
                assert_eq!(String::from_utf8(t).unwrap().trim(), kind.name());
                let content = g.run(&["cat-file", kind.name(), &hex], b"");
                assert!(&content == data, "{oid} level {level}");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(odb.loose().path_of(oid))
                        .unwrap()
                        .permissions()
                        .mode();
                    assert_eq!(mode & 0o222, 0, "loose objects are read-only");
                }
            }
            let fsck = g.raw(&["fsck", "--strict", "--no-dangling"], b"");
            assert!(
                fsck.status.success() && fsck.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&fsck.stderr)
            );
            // Level 0 stores the data uncompressed.
            if level == 0 {
                let (oid, _, data) = &written[written.len() - 4];
                let size = std::fs::metadata(odb.loose().path_of(oid)).unwrap().len();
                assert!(size > data.len() as u64);
            }
        }
    }
}

#[test]
fn existing_objects_are_freshened_not_rewritten() {
    let Some(bin) = reference() else { return };
    let g = repo(&bin, "freshen", HashKind::Sha1);
    let oid = g.oid(&["hash-object", "-w", "--stdin"], b"fresh\n");
    let odb = ObjectDatabase::open(g.objects(), HashKind::Sha1, opts(9));
    let path = odb.loose().path_of(&oid);
    let before = std::fs::read(&path).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(978_307_200);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert_eq!(odb.write(ObjectKind::Blob, b"fresh\n").unwrap(), oid);
    assert_eq!(std::fs::read(&path).unwrap(), before, "not rewritten");
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(mtime > old + std::time::Duration::from_secs(3600));
}

#[test]
fn prefix_lookup_matches_disambiguate() {
    let Some(bin) = reference() else { return };
    for hash in HashKind::ALL {
        let g = repo(&bin, "prefix", hash);
        let odb = ObjectDatabase::open(g.objects(), hash, opts(1));
        let mut ids = Vec::new();
        for i in 0..3000 {
            ids.push(
                odb.write(ObjectKind::Blob, format!("blob {i}\n").as_bytes())
                    .unwrap(),
            );
        }
        // Prefixes of written objects (some shared by several objects) plus
        // a few that match nothing.
        let mut prefixes: BTreeSet<String> = ids
            .iter()
            .take(200)
            .flat_map(|id| (4..=6).map(move |n| id.to_hex_with_len(n)))
            .collect();
        prefixes.extend(["0000", "ffff", "abcd", "1234"].map(String::from));
        let mut ambiguous = 0;
        for p in &prefixes {
            let out = g.raw(&["rev-parse", &format!("--disambiguate={p}")], b"");
            let theirs: Vec<ObjectId> = String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .map(|l| l.parse().unwrap())
                .collect();
            let prefix = Prefix::from_hex(hash, p.as_bytes()).unwrap();
            let expected = match theirs.len() {
                0 => PrefixMatch::None,
                1 => PrefixMatch::Unique(theirs[0]),
                _ => {
                    ambiguous += 1;
                    PrefixMatch::Ambiguous(theirs)
                }
            };
            assert_eq!(odb.resolve_prefix(&prefix).unwrap(), expected, "{p}");
        }
        assert!(ambiguous > 0);
        // Short prefixes (below git's minimum) still work in the library.
        let one = Prefix::from_hex(hash, &ids[0].to_hex().as_bytes()[..1]).unwrap();
        let PrefixMatch::Ambiguous(v) = odb.resolve_prefix(&one).unwrap() else {
            panic!("1-digit prefix is ambiguous among 3000 objects");
        };
        assert!(v.contains(&ids[0]) && v.iter().all(|id| id.starts_with(&one)));
    }
}

#[test]
fn shared_repository_modes_match_git() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Some(bin) = reference() else { return };
        for shared in ["umask", "group", "all", "0640", "0660", "0600", "0664"] {
            let g = repo(&bin, "shared-git", HashKind::Sha1);
            g.run(&["config", "core.sharedRepository", shared], b"");
            let oid = g.oid(&["hash-object", "-w", "--stdin"], shared.as_bytes());
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
            let git_file = mode(
                &g.objects()
                    .join(&oid.to_hex()[..2])
                    .join(&oid.to_hex()[2..]),
            );
            let git_dir = mode(&g.objects().join(&oid.to_hex()[..2]));

            let r = repo(&bin, "shared-rgit", HashKind::Sha1);
            let odb = ObjectDatabase::open(
                r.objects(),
                HashKind::Sha1,
                LooseOptions {
                    shared: SharedRepository::parse(shared).unwrap(),
                    ..LooseOptions::default()
                },
            );
            let ours = odb.write(ObjectKind::Blob, shared.as_bytes()).unwrap();
            assert_eq!(ours, oid);
            let path = odb.loose().path_of(&oid);
            assert_eq!(mode(&path), git_file, "file mode, shared={shared}");
            assert_eq!(
                mode(path.parent().unwrap()),
                git_dir,
                "dir mode, shared={shared}"
            );
        }
    }
}

#[test]
fn options_from_config() {
    let cfg = |text: &str| parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Local).unwrap();
    let mut w = Vec::new();
    let o = LooseOptions::from_config(&cfg(""), &mut w).unwrap();
    assert_eq!(o.compression, 1);
    assert_eq!(o.fsync, None);
    let o = LooseOptions::from_config(&cfg("[core]\ncompression = 9\n"), &mut w).unwrap();
    assert_eq!(o.compression, 9);
    let o = LooseOptions::from_config(
        &cfg("[core]\ncompression = 9\nlooseCompression = 0\n"),
        &mut w,
    )
    .unwrap();
    assert_eq!(o.compression, 0);
    let o = LooseOptions::from_config(&cfg("[core]\ncompression = -1\n"), &mut w).unwrap();
    assert_eq!(o.compression, 6);
    let e = LooseOptions::from_config(&cfg("[core]\ncompression = 12\n"), &mut w).unwrap_err();
    assert_eq!(e.to_string(), "bad zlib compression level 12");
    let e = LooseOptions::from_config(&cfg("[core]\nlooseCompression = -2\n"), &mut w).unwrap_err();
    assert_eq!(e.to_string(), "bad zlib compression level -2");
    let o = LooseOptions::from_config(
        &cfg(
            "[core]\nfsync = loose-object\nfsyncMethod = writeout-only\nsharedRepository = group\n",
        ),
        &mut w,
    )
    .unwrap();
    assert_eq!(o.fsync, Some(rgit_fs::FsyncMethod::WriteoutOnly));
    assert_eq!(o.shared, SharedRepository::Group);
    let o = LooseOptions::from_config(&cfg("[core]\nfsyncObjectFiles = true\n"), &mut w).unwrap();
    assert!(o.fsync.is_some());
    assert!(w.is_empty());
    LooseOptions::from_config(&cfg("[core]\nfsync = bogus\nfsyncMethod = bogus\n"), &mut w)
        .unwrap();
    assert_eq!(
        w,
        [
            "ignoring unknown core.fsync component 'bogus'",
            "ignoring unknown core.fsyncMethod value 'bogus'"
        ]
    );
}

/// Write `file` as the loose file of a made-up id and return the id.
fn plant(g: &Git, hash: HashKind, n: u32, file: &[u8]) -> ObjectId {
    let oid = rgit_hash::hash(hash, format!("planted {n}").as_bytes()).unwrap();
    let hex = oid.to_hex();
    let dir = g.objects().join(&hex[..2]);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&hex[2..]), file).unwrap();
    oid
}

fn deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// git's `error:` lines, with the repository path removed.
fn git_errors(g: &Git, stderr: &[u8], oid: &ObjectId) -> Vec<String> {
    let _ = oid;
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|l| {
            l.strip_prefix("error: ")
                .or_else(|| l.strip_prefix("fatal: "))
        })
        .filter(|l| !l.starts_with("git cat-file"))
        .map(|l| l.replace(&format!("{}/", g.dir.display()), ""))
        .collect()
}

fn our_lines(g: &Git, e: &Error) -> Vec<String> {
    e.git_lines()
        .into_iter()
        .map(|l| l.replace(&format!("{}/", g.dir.display()), ""))
        .collect()
}

#[test]
fn corrupt_objects_report_git_messages() {
    let Some(bin) = reference() else { return };
    for hash in HashKind::ALL {
        let g = repo(&bin, "corrupt", hash);
        let good = deflate(b"blob 5\0hello");
        let mut flipped = good.clone();
        flipped[10] = 0;
        let header_cases: Vec<Vec<u8>> = vec![
            flipped,
            b"blob 5\0hello".to_vec(),
            deflate(b"blob 5hello"),
            deflate(b"blob 05\0hello"),
            deflate(b"\0\0\0"),
            deflate(b"blob  5\0hello"),
            deflate(&[b"blob 5".as_slice(), &[b' '; 40], b"\0hello"].concat()),
            Vec::new(),
            good[..2].to_vec(),
            good[..6].to_vec(),
        ];
        // Use a store with a relative path so messages print like git's.
        let store = LooseStore::new(g.objects(), hash, LooseOptions::default());
        for (i, file) in header_cases.iter().enumerate() {
            let oid = plant(&g, hash, i as u32, file);
            let out = g.raw(&["cat-file", "-t", &oid.to_hex()], b"");
            assert!(!out.status.success());
            let theirs = git_errors(&g, &out.stderr, &oid);
            let e = store.header(&oid).unwrap_err();
            assert_eq!(our_lines(&g, &e), theirs, "case {i}");
            assert!(store.read(&oid).is_err());
        }

        let oid = plant(&g, hash, 100, &deflate(b"bogus 5\0hello"));
        let out = g.raw(&["cat-file", "-t", &oid.to_hex()], b"");
        assert_eq!(git_errors(&g, &out.stderr, &oid), ["invalid object type"]);
        assert_eq!(
            store.header(&oid).unwrap_err().to_string(),
            "invalid object type"
        );

        // fsck-only problems: bytes after the stream, truncation, wrong name.
        let fsck_cases: Vec<(Vec<u8>, &str)> = vec![
            ([good.clone(), b"xyz".to_vec()].concat(), "garbage"),
            (good[..good.len() - 4].to_vec(), "truncated"),
            (good.clone(), "misnamed"),
        ];
        for (i, (file, what)) in fsck_cases.into_iter().enumerate() {
            let oid = plant(&g, hash, 200 + i as u32, &file);
            let out = g.raw(&["fsck"], b"");
            let text =
                String::from_utf8_lossy(&out.stderr).replace(&format!("{}/", g.dir.display()), "");
            let e = store.verify(&oid).unwrap_err();
            let ours = match &e {
                Error::HashMismatch { actual, path, .. } => {
                    rgit_odb::messages::hash_path_mismatch(actual, path)
                }
                e => e.to_string(),
            };
            let ours = ours.replace(&format!("{}/", g.dir.display()), "");
            assert!(
                text.lines().any(|l| l == format!("error: {ours}")),
                "{what}: {ours:?} not in\n{text}"
            );
            std::fs::remove_file(store.path_of(&oid)).unwrap();
        }
    }
}

#[test]
fn lookup_falls_back_to_next_store() {
    let Some(bin) = reference() else { return };
    // Like an alternate: the object is corrupt in the first store and
    // intact in the second.
    let a = repo(&bin, "alt-a", HashKind::Sha1);
    let b = repo(&bin, "alt-b", HashKind::Sha1);
    let oid = a.oid(&["hash-object", "-w", "--stdin"], b"shared\n");
    let good = std::fs::read(
        LooseStore::new(a.objects(), HashKind::Sha1, LooseOptions::default()).path_of(&oid),
    )
    .unwrap();
    let mut bad = good.clone();
    bad[1] ^= 0xff;
    let hex = oid.to_hex();
    std::fs::create_dir_all(b.objects().join(&hex[..2])).unwrap();
    std::fs::write(b.objects().join(&hex[..2]).join(&hex[2..]), &bad).unwrap();

    let mut odb = ObjectDatabase::open(b.objects(), HashKind::Sha1, LooseOptions::default());
    odb.push_store(Arc::new(LooseStore::new(
        a.objects(),
        HashKind::Sha1,
        LooseOptions::default(),
    )));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    odb.set_error_sink(move |e| s.lock().unwrap().extend(e.git_lines()));
    let obj = odb.read(&oid).unwrap().unwrap();
    assert_eq!(obj.data, b"shared\n");
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|l| l.starts_with("inflate: data stream error")),
        "{seen:?}"
    );

    // Without the good copy the error comes back.
    let alone = ObjectDatabase::open(b.objects(), HashKind::Sha1, LooseOptions::default());
    assert!(alone.read(&oid).is_err());
}

#[test]
fn hash_check_on_read() {
    let Some(bin) = reference() else { return };
    let g = repo(&bin, "hashcheck", HashKind::Sha1);
    let oid = plant(&g, HashKind::Sha1, 1, &deflate(b"blob 5\0hello"));
    let lenient = LooseStore::new(g.objects(), HashKind::Sha1, LooseOptions::default());
    assert_eq!(lenient.read(&oid).unwrap().unwrap().data, b"hello");
    let strict = LooseStore::new(
        g.objects(),
        HashKind::Sha1,
        LooseOptions {
            check_hash: true,
            ..LooseOptions::default()
        },
    );
    let e = strict.read(&oid).unwrap_err();
    assert_eq!(e.to_string(), format!("hash mismatch {oid}"));
}
