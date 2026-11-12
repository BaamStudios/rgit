// SPDX-License-Identifier: MIT

//! Format and API tests that do not need the git binary.

use rgit_hash::{HashKind, ObjectId};
use rgit_index::{
    AddError, AddOptions, CacheTree, Entry, Error, Flags, Index, Mode, OrderError, ReadOptions,
    Stage, Stat, Time, TreeNode, WriteOptions,
};

fn id(kind: HashKind, n: u8) -> ObjectId {
    let mut id = kind.null();
    id.as_bytes_mut().fill(n);
    id
}

fn entry(kind: HashKind, path: &str, stage: u8) -> Entry {
    let mut e = Entry::new(path, Stage::new(stage).unwrap(), Mode::FILE, id(kind, 7));
    e.stat = Stat {
        ctime: Time { secs: 1, nsecs: 2 },
        mtime: Time { secs: 3, nsecs: 4 },
        dev: 5,
        ino: 6,
        uid: 1000,
        gid: 100,
        size: 42,
    };
    e
}

fn sample(kind: HashKind) -> Index {
    let mut index = Index::new(kind);
    for p in [
        "a/b/c",
        "a/b/d",
        "a/e",
        "abc",
        "b",
        "very/long/common/prefix/x",
    ] {
        index.add(entry(kind, p, 0), AddOptions::default()).unwrap();
    }
    let mut root = TreeNode::new_valid("", 6, id(kind, 1));
    root.children.push(TreeNode::new_invalid("a"));
    index.set_cache_tree(Some(CacheTree { root }));
    index
}

fn parse(data: &[u8], kind: HashKind) -> Result<Index, Error> {
    Index::from_bytes(data, kind, &ReadOptions::default())
}

fn with_checksum(mut data: Vec<u8>, kind: HashKind) -> Vec<u8> {
    let sum = rgit_hash::hash(kind, &data).unwrap();
    data.extend_from_slice(sum.as_bytes());
    data
}

fn body(data: &[u8], kind: HashKind) -> Vec<u8> {
    data[..data.len() - kind.byte_len()].to_vec()
}

#[test]
fn round_trip_all_versions() {
    for kind in HashKind::ALL {
        for version in [2, 4] {
            let mut index = sample(kind);
            index.set_version(version);
            let bytes = index.to_bytes(&WriteOptions::default());
            assert_eq!(&bytes[4..8], &version.to_be_bytes());
            let back = parse(&bytes, kind).unwrap();
            assert_eq!(back.entries(), index.entries());
            assert_eq!(back.cache_tree(), index.cache_tree());
            assert_eq!(back.to_bytes(&WriteOptions::default()), bytes);
        }
    }
}

#[test]
fn v2_entry_layout() {
    let kind = HashKind::Sha1;
    let mut index = Index::new(kind);
    index
        .add(entry(kind, "abc", 0), AddOptions::default())
        .unwrap();
    let bytes = index.to_bytes(&WriteOptions::default());
    // 12 header + 62 fixed + 3 path + 5 NUL padding (to 72) + 20 checksum.
    assert_eq!(bytes.len(), 12 + 72 + 20);
    assert_eq!(&bytes[12 + 60..12 + 62], &[0, 3]);
    assert_eq!(&bytes[12 + 62..12 + 72], b"abc\0\0\0\0\0\0\0");
}

#[test]
fn extended_flags_need_version_3() {
    let kind = HashKind::Sha1;
    let mut index = sample(kind);
    assert_eq!(index.write_version(), 2);
    let pos = index.position(b"b", Stage::MERGED).unwrap();
    index.entry_mut(pos).unwrap().flags = Flags {
        assume_valid: true,
        skip_worktree: true,
        intent_to_add: true,
    };
    assert_eq!(index.write_version(), 3);
    let bytes = index.to_bytes(&WriteOptions::default());
    let back = parse(&bytes, kind).unwrap();
    assert_eq!(back.version(), 3);
    let e = back.get(b"b", Stage::MERGED).unwrap();
    assert!(e.flags.assume_valid && e.flags.skip_worktree && e.flags.intent_to_add);
    assert_eq!(e.debug_flags(), 0x6000_c000);
    // Back to version 2 once no entry needs extended flags.
    let mut back = back;
    back.entry_mut(pos).unwrap().flags = Flags::default();
    assert_eq!(back.write_version(), 2);
}

#[test]
fn long_paths() {
    let kind = HashKind::Sha1;
    let long = "x".repeat(5000);
    for version in [2, 4] {
        let mut index = Index::new(kind);
        index.set_version(version);
        index
            .add(entry(kind, &long, 0), AddOptions::default())
            .unwrap();
        index
            .add(entry(kind, &(long.clone() + "y"), 0), AddOptions::default())
            .unwrap();
        let bytes = index.to_bytes(&WriteOptions::default());
        let back = parse(&bytes, kind).unwrap();
        assert_eq!(back.entries(), index.entries());
    }
}

#[test]
fn checksum() {
    let kind = HashKind::Sha1;
    let bytes = sample(kind).to_bytes(&WriteOptions::default());
    let index = parse(&bytes, kind).unwrap();
    assert!(index.checksum().is_some());

    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 1;
    // Not verified by default (as in git), only on request.
    assert!(parse(&bad, kind).is_ok());
    assert!(matches!(
        Index::from_bytes(&bad, kind, &ReadOptions::verified()),
        Err(Error::BadChecksum)
    ));
    assert!(Index::from_bytes(&bytes, kind, &ReadOptions::verified()).is_ok());

    // index.skipHash: null trailer written and accepted.
    let skip = sample(kind).to_bytes(&WriteOptions { skip_hash: true });
    assert!(skip.ends_with(&[0; 20]));
    let read = Index::from_bytes(&skip, kind, &ReadOptions::verified()).unwrap();
    assert!(read.checksum().is_none());
}

#[test]
fn header_errors() {
    let kind = HashKind::Sha1;
    let bytes = sample(kind).to_bytes(&WriteOptions::default());
    assert!(matches!(parse(&bytes[..31], kind), Err(Error::TooSmall)));
    // A SHA-1 index read as SHA-256 has a wrong checksum.
    assert!(Index::from_bytes(&bytes, HashKind::Sha256, &ReadOptions::verified()).is_err());

    let mut b = body(&bytes, kind);
    b[..4].copy_from_slice(b"DIRX");
    let err = parse(&with_checksum(b, kind), kind).unwrap_err();
    assert!(matches!(err, Error::BadSignature(_)));
    if cfg!(target_endian = "little") {
        assert_eq!(
            err.report("i"),
            [
                "error: bad signature 0x58524944",
                "fatal: index file corrupt"
            ]
        );
    }

    let mut b = body(&bytes, kind);
    b[4..8].copy_from_slice(&5u32.to_be_bytes());
    let err = parse(&with_checksum(b, kind), kind).unwrap_err();
    assert_eq!(
        err.report("i"),
        ["error: bad index version 5", "fatal: index file corrupt"]
    );

    let err = parse(&bytes[..20], kind).unwrap_err();
    assert_eq!(
        err.report(".git/index"),
        ["fatal: .git/index: index file smaller than expected"]
    );
}

#[test]
fn truncated_and_bad_counts() {
    let kind = HashKind::Sha1;
    let bytes = sample(kind).to_bytes(&WriteOptions::default());
    let mut b = body(&bytes, kind);
    b[8..12].copy_from_slice(&1000u32.to_be_bytes());
    assert!(parse(&with_checksum(b, kind), kind).is_err());
    for cut in [13, 40, 80, 100] {
        let b = body(&bytes, kind)[..cut].to_vec();
        assert!(parse(&with_checksum(b, kind), kind).is_err(), "{cut}");
    }
}

#[test]
fn unknown_extended_flag_bits() {
    let kind = HashKind::Sha1;
    let mut index = Index::new(kind);
    let mut e = entry(kind, "a", 0);
    e.flags.skip_worktree = true;
    index.add(e, AddOptions::default()).unwrap();
    let bytes = index.to_bytes(&WriteOptions::default());
    let mut b = body(&bytes, kind);
    // Extended flags follow the 16-bit flags at 12 + 62.
    b[12 + 62] = 0x80;
    let err = parse(&with_checksum(b, kind), kind).unwrap_err();
    assert_eq!(
        err.report("i"),
        ["fatal: unknown index entry format 0x80000000"]
    );
}

fn append_ext(bytes: &[u8], kind: HashKind, sig: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut b = body(bytes, kind);
    b.extend_from_slice(sig);
    b.extend_from_slice(&(data.len() as u32).to_be_bytes());
    b.extend_from_slice(data);
    with_checksum(b, kind)
}

#[test]
fn extensions() {
    let kind = HashKind::Sha1;
    let index = sample(kind);
    let bytes = index.to_bytes(&WriteOptions::default());

    // Unknown optional: ignored, dropped on write.
    let data = append_ext(&bytes, kind, b"ZZZZ", b"whatever");
    let read = parse(&data, kind).unwrap();
    assert_eq!(read.ignored_extensions(), [*b"ZZZZ"]);
    assert_eq!(read.to_bytes(&WriteOptions::default()), bytes);

    // Known optional extensions rgit does not keep.
    for sig in [b"UNTR", b"FSMN", b"EOIE", b"IEOT"] {
        let read = parse(&append_ext(&bytes, kind, sig, b"x"), kind).unwrap();
        assert_eq!(read.dropped_extensions(), [*sig]);
        assert_eq!(read.to_bytes(&WriteOptions::default()), bytes);
    }

    // REUC is kept as is.
    let reuc = b"path\x00100644\x000\x000\x00".to_vec();
    let with_reuc = append_ext(&bytes, kind, b"REUC", &reuc);
    let read = parse(&with_reuc, kind).unwrap();
    assert_eq!(read.resolve_undo_raw(), Some(&reuc[..]));
    assert_eq!(read.to_bytes(&WriteOptions::default()), with_reuc);

    // Required extensions.
    let err = parse(&append_ext(&bytes, kind, b"abcd", b""), kind).unwrap_err();
    assert_eq!(
        err.report("i"),
        [
            "error: index uses abcd extension, which we do not understand",
            "fatal: index file corrupt"
        ]
    );
    assert!(matches!(
        parse(&append_ext(&bytes, kind, b"link", &[0; 20]), kind),
        Err(Error::SplitIndex)
    ));
    assert!(matches!(
        parse(&append_ext(&bytes, kind, b"sdir", b""), kind),
        Err(Error::SparseIndex)
    ));

    // Optional extension larger than the file: ignored like git does.
    let mut b = body(&bytes, kind);
    b.extend_from_slice(b"ABCD");
    b.extend_from_slice(&1000u32.to_be_bytes());
    b.extend_from_slice(b"xyz");
    let read = parse(&with_checksum(b, kind), kind).unwrap();
    assert_eq!(read.ignored_extensions(), [*b"ABCD"]);
    assert_eq!(read.to_bytes(&WriteOptions::default()), bytes);
    // A required one is still refused.
    let mut b = body(&bytes, kind);
    b.extend_from_slice(b"abcd");
    b.extend_from_slice(&1000u32.to_be_bytes());
    assert!(matches!(
        parse(&with_checksum(b, kind), kind),
        Err(Error::UnknownExtension(_))
    ));

    // A malformed TREE is dropped, not fatal.
    let mut no_tree = index.clone();
    no_tree.set_cache_tree(None);
    let plain = no_tree.to_bytes(&WriteOptions::default());
    let read = parse(&append_ext(&plain, kind, b"TREE", b"garbage"), kind).unwrap();
    assert!(read.cache_tree().is_none());
}

#[test]
fn lookup_and_ranges() {
    let kind = HashKind::Sha1;
    let mut index = sample(kind);
    assert_eq!(index.dir_range(b"a"), 0..3);
    assert_eq!(index.dir_range(b"a/b"), 0..2);
    assert_eq!(index.dir_range(b"a/b/"), 0..2);
    assert_eq!(index.dir_range(b"ab"), 3..3);
    assert_eq!(index.dir_range(b""), 0..6);
    assert!(index.get(b"a/e", Stage::MERGED).is_some());
    assert!(index.get(b"a/e", Stage::OURS).is_none());

    // Conflict stages.
    index.remove(b"b", Stage::MERGED).unwrap();
    for stage in [3, 1, 2] {
        index
            .add(entry(kind, "b", stage), AddOptions::default())
            .unwrap();
    }
    assert!(index.has_conflicts());
    let stages: Vec<u8> = index
        .entries_for_path(b"b")
        .iter()
        .map(|e| e.stage().get())
        .collect();
    assert_eq!(stages, [1, 2, 3]);
    assert_eq!(index.unmerged().count(), 1);
    index.verify_order().unwrap();
    assert_eq!(index.remove_path(b"b").len(), 3);
    assert!(!index.has_conflicts());
}

#[test]
fn file_directory_conflicts() {
    let kind = HashKind::Sha1;
    let mut index = sample(kind);
    let plain = AddOptions::default();
    let replace = AddOptions {
        ok_to_replace: true,
        ..plain
    };
    assert_eq!(
        index.add(entry(kind, "a/b", 0), plain),
        Err(AddError::FileDirectoryConflict(b"a/b".to_vec()))
    );
    assert!(index.add(entry(kind, "abc/x", 0), plain).is_err());
    // Other stages do not conflict.
    index.add(entry(kind, "abc/x", 2), plain).unwrap();
    index.remove(b"abc/x", Stage::OURS).unwrap();

    index.add(entry(kind, "a/b", 0), replace).unwrap();
    assert_eq!(index.dir_range(b"a/b"), 1..1);
    assert!(index.get(b"a/b", Stage::MERGED).is_some());
    index.add(entry(kind, "abc/x/y", 0), replace).unwrap();
    assert!(index.get(b"abc", Stage::MERGED).is_none());

    for bad in ["", "/a", "a/", "a//b", "./a", "a/../b", ".git/x", "x/.GIT"] {
        assert_eq!(
            index.add(entry(kind, bad, 0), plain),
            Err(AddError::InvalidPath(bad.as_bytes().to_vec())),
            "{bad}"
        );
    }
    assert_eq!(
        AddError::InvalidPath(b".git/x".to_vec()).to_string(),
        "Invalid path '.git/x'"
    );
}

#[test]
fn edits_invalidate_cache_tree() {
    let kind = HashKind::Sha1;
    let mut index = sample(kind);
    let mut root = TreeNode::new_valid("", 6, id(kind, 1));
    let mut a = TreeNode::new_valid("a", 3, id(kind, 2));
    a.children.push(TreeNode::new_valid("b", 2, id(kind, 3)));
    root.children.push(a);
    root.children
        .push(TreeNode::new_valid("very", 1, id(kind, 4)));
    index.set_cache_tree(Some(CacheTree { root }));

    index.remove(b"a/e", Stage::MERGED).unwrap();
    let tree = index.cache_tree().unwrap();
    assert!(!tree.root.is_valid());
    assert!(!tree.find(b"a").unwrap().is_valid());
    assert!(tree.find(b"a/b").unwrap().is_valid());
    assert!(tree.find(b"very").unwrap().is_valid());

    let pos = index.position(b"a/b/c", Stage::MERGED).unwrap();
    index.entry_mut(pos).unwrap().stat.size = 1;
    assert!(index.cache_tree().unwrap().find(b"a/b").unwrap().is_valid());
    index.invalidate_path(b"a/b/c");
    assert!(!index.cache_tree().unwrap().find(b"a/b").unwrap().is_valid());
}

#[test]
fn verify_order_messages() {
    let kind = HashKind::Sha1;
    let build = |entries: &[(&str, u8)]| {
        // Write out of order by building version 2 bytes by hand through
        // a sorted index and swapping, which the reader accepts.
        let mut data = Vec::new();
        data.extend_from_slice(b"DIRC");
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        for (path, stage) in entries {
            let mut one = Index::new(kind);
            one.add(entry(kind, path, *stage), AddOptions::default())
                .unwrap();
            let bytes = one.to_bytes(&WriteOptions::default());
            data.extend_from_slice(&bytes[12..bytes.len() - 20]);
        }
        parse(&with_checksum(data, kind), kind).unwrap()
    };
    assert_eq!(
        build(&[("b", 0), ("a", 0)]).verify_order(),
        Err(OrderError::Unordered)
    );
    assert_eq!(
        build(&[("a", 0), ("a", 1)])
            .verify_order()
            .unwrap_err()
            .to_string(),
        "multiple stage entries for merged file 'a'"
    );
    assert_eq!(
        build(&[("a", 2), ("a", 1)])
            .verify_order()
            .unwrap_err()
            .to_string(),
        "unordered stage entries for 'a'"
    );
    assert!(build(&[("a", 1), ("a", 1)]).verify_order().is_ok());
    assert!(
        build(&[("a", 1), ("a", 3), ("b", 0)])
            .verify_order()
            .is_ok()
    );
}

#[test]
fn file_io() {
    let dir = std::env::temp_dir().join(format!("rgit-index-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("index");
    let kind = HashKind::Sha256;
    let missing = Index::read_file(&path, kind, &ReadOptions::default()).unwrap();
    assert!(missing.is_empty());
    let index = sample(kind);
    index
        .write_file(
            &path,
            &rgit_fs::LockOptions::new(),
            &WriteOptions::default(),
        )
        .unwrap();
    assert!(!dir.join("index.lock").exists());
    let back = Index::read_file(&path, kind, &ReadOptions::default()).unwrap();
    assert_eq!(back.entries(), index.entries());

    // A held lock makes the write fail with git's message.
    let lock = rgit_fs::LockFile::acquire(&path, &rgit_fs::LockOptions::new()).unwrap();
    let err = index
        .write_file(
            &path,
            &rgit_fs::LockOptions::new(),
            &WriteOptions::default(),
        )
        .unwrap_err();
    assert!(err.to_string().starts_with("Unable to create"), "{err}");
    drop(lock);
    std::fs::remove_dir_all(&dir).unwrap();
}
