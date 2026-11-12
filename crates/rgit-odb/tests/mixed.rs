// SPDX-License-Identifier: MIT

//! The full object database (packs + loose objects + alternates) against
//! the reference git: every object of a repository with packed (deltified)
//! and loose objects and an alternate reads back exactly as
//! `git cat-file --batch-all-objects --batch` shows it; a corrupt packed
//! copy is reported with git's `error:` lines, and a corrupt loose copy is
//! reported before the packed copy is used, as git does.

mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use common::{Git, reference, repo};
use rgit_hash::{HashKind, ObjectId, Prefix};
use rgit_object::{ObjectHeader, ObjectKind};
use rgit_odb::{LooseOptions, LooseStore, ObjectDatabase, PrefixMatch};

/// Commits with similar file contents, so that `repack -f` makes deltas.
fn history(g: &Git, tag: &str, rounds: usize) {
    for i in 0..rounds {
        let mut text = String::new();
        for line in 0..200 {
            text.push_str(&format!("{tag} line {line} of a long file\n"));
        }
        text.push_str(&format!("round {i}\n"));
        std::fs::write(g.dir.join("file.txt"), text).unwrap();
        std::fs::write(g.dir.join(format!("small-{i}")), format!("{tag} {i}\n")).unwrap();
        g.run(&["add", "."], b"");
        g.run(&["commit", "-q", "-m", &format!("{tag} {i}")], b"");
    }
}

/// `git cat-file --batch-all-objects --batch`, parsed.
fn all_objects(g: &Git) -> Vec<(ObjectId, ObjectKind, Vec<u8>)> {
    let out = g.run(&["cat-file", "--batch-all-objects", "--batch"], b"");
    let mut v = Vec::new();
    let mut rest = &out[..];
    while !rest.is_empty() {
        let nl = rest.iter().position(|&b| b == b'\n').unwrap();
        let head = std::str::from_utf8(&rest[..nl]).unwrap();
        let mut f = head.split(' ');
        let id: ObjectId = f.next().unwrap().parse().unwrap();
        let kind: ObjectKind = f.next().unwrap().parse().unwrap();
        let size: usize = f.next().unwrap().parse().unwrap();
        let data = rest[nl + 1..nl + 1 + size].to_vec();
        rest = &rest[nl + 1 + size + 1..];
        v.push((id, kind, data));
    }
    v
}

fn pack_count(g: &Git) -> usize {
    std::fs::read_dir(g.objects().join("pack"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".pack"))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn packed_loose_and_alternate_objects_read_like_git() {
    let Some(bin) = reference() else { return };
    for hash in HashKind::ALL {
        // The alternate: fully packed.
        let base = repo(&bin, "mixed-base", hash);
        history(&base, "base", 4);
        base.run(&["repack", "-adfq"], b"");
        assert_eq!(pack_count(&base), 1);

        // The repository: borrows from `base`, has its own pack (with
        // deltas) and loose objects on top.
        let g = repo(&bin, "mixed", hash);
        std::fs::create_dir_all(g.objects().join("info")).unwrap();
        std::fs::write(
            g.objects().join("info/alternates"),
            format!("{}\n", base.objects().display()),
        )
        .unwrap();
        g.run(&["fetch", "-q", base.dir.to_str().unwrap(), "HEAD"], b"");
        g.run(&["reset", "-q", "--hard", "FETCH_HEAD"], b"");
        history(&g, "own", 3);
        g.run(&["repack", "-dfq"], b"");
        history(&g, "loose", 2);
        assert_eq!(pack_count(&g), 1);

        let odb = ObjectDatabase::open_with_packs(g.objects(), hash, LooseOptions::default());
        let theirs = all_objects(&g);
        assert!(theirs.len() > 30, "{}", theirs.len());

        let ids: BTreeSet<ObjectId> = theirs.iter().map(|o| o.0).collect();
        assert_eq!(odb.all_objects().unwrap(), ids);

        let mut loose = 0;
        for (id, kind, data) in &theirs {
            if odb.loose().contains(id) {
                loose += 1;
            }
            assert!(odb.contains(id));
            assert_eq!(
                odb.header(id).unwrap().unwrap(),
                ObjectHeader {
                    kind: *kind,
                    size: data.len() as u64
                },
                "{id}"
            );
            let raw = odb.read(id).unwrap().unwrap();
            assert_eq!(raw.kind, *kind, "{id}");
            assert_eq!(&raw.data, data, "{id}");
            odb.read_object(id).unwrap().unwrap();

            let hex = id.to_hex();
            let p = Prefix::from_hex(hash, &hex.as_bytes()[..16]).unwrap();
            assert_eq!(odb.resolve_prefix(&p).unwrap(), PrefixMatch::Unique(*id));
        }
        // All three kinds of storage are really involved.
        assert!(loose > 0 && loose < theirs.len());
        assert!(!odb.packs().unwrap().set().packs().is_empty());

        let missing = rgit_hash::hash_object(hash, "blob", b"not there\n").unwrap();
        assert!(odb.read(&missing).unwrap().is_none());
        assert!(odb.header(&missing).unwrap().is_none());
    }
}

/// `error:` lines of git's stderr.
fn git_errors(out: &std::process::Output) -> Vec<String> {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.starts_with("error: "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn corrupt_copies_are_reported_like_git() {
    let Some(bin) = reference() else { return };
    let g = repo(&bin, "mixed-corrupt", HashKind::Sha1);
    // Non-delta content, big enough to damage its middle.
    let content: Vec<u8> = (0..4000u32)
        .flat_map(|i| (i.wrapping_mul(2_654_435_761)).to_le_bytes())
        .collect();
    let oid = g.oid(&["hash-object", "-w", "--stdin"], &content);
    let hex = oid.to_hex();
    g.run(&["update-ref", "refs/keep", &hex], b"");
    g.run(&["repack", "-adq"], b"");
    let loose = LooseStore::new(g.objects(), HashKind::Sha1, LooseOptions::default());
    assert!(!loose.contains(&oid));

    // Damage the zlib stream of the blob inside the pack.
    let pack = std::fs::read_dir(g.objects().join("pack"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "pack"))
        .unwrap();
    let idx = pack.with_extension("idx");
    let listing =
        String::from_utf8(g.run(&["verify-pack", "-v", idx.to_str().unwrap()], b"")).unwrap();
    let line = listing.lines().find(|l| l.starts_with(&hex)).unwrap();
    let offset: usize = line.split_whitespace().nth(4).unwrap().parse().unwrap();
    let good_pack = std::fs::read(&pack).unwrap();
    let mut bytes = good_pack.clone();
    // Past the entry header and the zlib header, inside the deflate data.
    for b in &mut bytes[offset + 8..offset + 40] {
        *b ^= 0x55;
    }
    set_writable(&pack);
    std::fs::write(&pack, &bytes).unwrap();

    let mut odb =
        ObjectDatabase::open_with_packs(g.objects(), HashKind::Sha1, LooseOptions::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    odb.set_error_sink(move |e| {
        let lines = e
            .skipped_copy_lines()
            .into_iter()
            .map(|l| format!("error: {l}"));
        s.lock().unwrap().extend(lines)
    });
    let take = || std::mem::take(&mut *seen.lock().unwrap());

    // 1. Only a corrupt packed copy.
    let out = g.raw(&["cat-file", "blob", &hex], b"");
    assert!(!out.status.success());
    assert!(odb.read(&oid).is_err());
    let ours = take();
    assert!(!ours.is_empty());
    assert_eq!(ours, git_errors(&out));

    // 2. Plus an intact loose copy: it is read first, nothing is reported.
    loose.write(ObjectKind::Blob, &content).unwrap();
    let out = g.raw(&["cat-file", "blob", &hex], b"");
    assert!(out.status.success());
    assert_eq!(odb.read(&oid).unwrap().unwrap().data, content);
    assert_eq!(take(), git_errors(&out));

    // 3. A corrupt loose copy and an intact packed one: the loose copy is
    // reported, the packed one used. Header lookups stay silent.
    std::fs::write(&pack, &good_pack).unwrap();
    let path = loose.path_of(&oid);
    set_writable(&path);
    std::fs::write(&path, b"garbage").unwrap();
    let out = g.raw(&["cat-file", "blob", &hex], b"");
    assert!(out.status.success());
    assert_eq!(odb.read(&oid).unwrap().unwrap().data, content);
    let ours = take();
    assert!(!ours.is_empty());
    assert_eq!(ours, git_errors(&out));

    let out = g.raw(&["cat-file", "-t", &hex], b"");
    assert!(out.status.success());
    assert_eq!(odb.header(&oid).unwrap().unwrap().kind, ObjectKind::Blob);
    assert_eq!(take(), git_errors(&out));
}

fn set_writable(p: &std::path::Path) {
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(p, perm).unwrap();
}
