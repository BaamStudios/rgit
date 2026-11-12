// SPDX-License-Identifier: MIT

//! Differential tests: indexes written by the reference git are read and
//! rewritten by rgit-index, and the results are compared with git's view
//! (`git ls-files --stage --debug`) and git's bytes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rgit_difftest::{Fixture, assert_same_text};
use rgit_fs::LockOptions;
use rgit_hash::{HashKind, ObjectId};
use rgit_index::{AddOptions, Entry, Index, Mode, ReadOptions, Stage, WriteOptions};

/// C-style quoting as `git ls-files` does with the default `core.quotePath`.
fn quote(path: &[u8]) -> String {
    let needs = path
        .iter()
        .any(|&b| !(0x20..0x7f).contains(&b) || b == b'"' || b == b'\\');
    if !needs {
        return String::from_utf8(path.to_vec()).unwrap();
    }
    let mut s = String::from("\"");
    for &b in path {
        match b {
            b'\x07' => s.push_str("\\a"),
            b'\x08' => s.push_str("\\b"),
            b'\t' => s.push_str("\\t"),
            b'\n' => s.push_str("\\n"),
            b'\x0b' => s.push_str("\\v"),
            b'\x0c' => s.push_str("\\f"),
            b'\r' => s.push_str("\\r"),
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            0x20..=0x7e => s.push(b as char),
            _ => write!(s, "\\{b:03o}").unwrap(),
        }
    }
    s.push('"');
    s
}

/// `git ls-files --stage --debug` computed from rgit's parsed index.
fn ls_files_debug(index: &Index) -> String {
    let mut out = String::new();
    for e in index.entries() {
        let s = &e.stat;
        writeln!(
            out,
            "{} {} {}\t{}",
            e.mode,
            e.id,
            e.stage(),
            quote(e.path())
        )
        .unwrap();
        writeln!(out, "  ctime: {}:{}", s.ctime.secs, s.ctime.nsecs).unwrap();
        writeln!(out, "  mtime: {}:{}", s.mtime.secs, s.mtime.nsecs).unwrap();
        writeln!(out, "  dev: {}\tino: {}", s.dev, s.ino).unwrap();
        writeln!(out, "  uid: {}\tgid: {}", s.uid, s.gid).unwrap();
        writeln!(out, "  size: {}\tflags: {:x}", s.size, e.debug_flags()).unwrap();
    }
    out
}

fn index_file(fx: &Fixture) -> PathBuf {
    fx.work_dir().join(".git/index")
}

fn git_ls(fx: &Fixture) -> String {
    String::from_utf8(fx.git_stdout(["ls-files", "--stage", "--debug"])).unwrap()
}

fn read(path: &Path, kind: HashKind) -> Index {
    Index::read_file(path, kind, &ReadOptions::default()).unwrap()
}

/// Reads git's index, compares rgit's view with git's, checks that an
/// unchanged rewrite is byte-identical, then lets rgit write the index
/// through the lock file and checks git's view of the result.
fn check_round_trip(fx: &Fixture, kind: HashKind, label: &str) -> Index {
    let path = index_file(fx);
    let original = std::fs::read(&path).unwrap();
    let index = Index::from_bytes(&original, kind, &ReadOptions::default()).unwrap();
    let expected = git_ls(fx);
    assert_same_text(label, &expected, &ls_files_debug(&index));
    let rewritten = index.to_bytes(&WriteOptions::default());
    assert!(
        rewritten == original,
        "{label}: rewrite is not byte-identical ({} vs {} bytes)",
        rewritten.len(),
        original.len()
    );
    index
        .write_file(&path, &LockOptions::new(), &WriteOptions::default())
        .unwrap();
    assert_same_text(label, &expected, &git_ls(fx));
    index
}

fn repo(kind: HashKind) -> Fixture {
    let fx = Fixture::new().git(["init", "-q", &format!("--object-format={}", kind.name())]);
    if fx.is_skipped() {
        return fx;
    }
    let fx = fx
        .git(["config", "core.splitIndex", "false"])
        .write("a/b/f", "1\n")
        .write("a/g", "2\n")
        .write("a-b", "dash\n")
        .write("a.b/c", "dot\n")
        .write("c/h", "3\n")
        .write("top", "4\n")
        .write("caf\u{e9}", "unicode\n")
        .write("sp ace", "space\n")
        .write_executable("bin/run", "#!/bin/sh\n")
        .symlink("top", "link");
    // A path longer than the 12-bit name length field (only in the index,
    // it is too long for the file system).
    let blob = String::from_utf8(fx.git_stdout(["hash-object", "-w", "top"])).unwrap();
    let long = format!("deep/{}", "x".repeat(4200));
    fx.git([
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("100644,{},{long}", blob.trim()),
    ])
    .git(["add", "."])
    .git(["commit", "-q", "-m", "one"])
    .tick()
}

fn rich_index(kind: HashKind) -> Option<Fixture> {
    let fx = repo(kind);
    if fx.is_skipped() {
        return None;
    }
    let gitlink = ObjectId::from_hex(kind, "1".repeat(kind.hex_len()).as_bytes()).unwrap();
    let fx = fx
        .git([
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{gitlink},sub"),
        ])
        .write("c/h", "changed\n")
        .git(["add", "c/h"]) // invalidates TREE for c and the root
        .write("new", "")
        .git(["add", "-N", "new"])
        .git(["update-index", "--skip-worktree", "a/g"])
        .git(["update-index", "--assume-unchanged", "top"]);
    Some(fx)
}

#[test]
fn round_trip_versions_sha1() {
    round_trip_versions(HashKind::Sha1);
}

#[test]
fn round_trip_versions_sha256() {
    round_trip_versions(HashKind::Sha256);
}

fn round_trip_versions(kind: HashKind) {
    let Some(fx) = rich_index(kind) else { return };
    let index = check_round_trip(&fx, kind, "v3 with extended flags");
    assert_eq!(index.version(), 3);
    let tree = index.cache_tree().expect("TREE extension");
    assert!(!tree.root.is_valid());
    // --skip-worktree a/g invalidated "a", but not "a/b".
    assert!(!tree.find(b"a").unwrap().is_valid());
    assert!(tree.find(b"a/b").unwrap().is_valid());
    assert!(!tree.find(b"c").unwrap().is_valid());

    let fx = fx.git(["update-index", "--index-version", "4"]);
    let index = check_round_trip(&fx, kind, "v4");
    assert_eq!(index.version(), 4);

    // Without extended flags git writes version 2.
    let fx = fx
        .git(["update-index", "--index-version", "2"])
        .git(["update-index", "--no-skip-worktree", "a/g"])
        .git(["rm", "-q", "--cached", "new"]);
    let index = check_round_trip(&fx, kind, "v2");
    assert_eq!(index.version(), 2);
    assert_eq!(index.write_version(), 2);
}

/// git's own rewrite (`--force-write-index`) of an unchanged index equals
/// rgit's rewrite.
#[test]
fn force_write_matches() {
    for kind in HashKind::ALL {
        let Some(fx) = rich_index(kind) else { return };
        let fx = fx
            .git(["update-index", "--no-skip-worktree", "a/g"])
            .git(["rm", "-q", "--cached", "new"]);
        let path = index_file(&fx);
        let index = read(&path, kind);
        let ours = index.to_bytes(&WriteOptions::default());
        let _fx = fx.git([
            "update-index",
            "--index-version",
            "2",
            "--force-write-index",
        ]);
        assert!(std::fs::read(&path).unwrap() == ours, "{kind}");
    }
}

#[test]
fn conflicts_and_resolve_undo() {
    let kind = HashKind::Sha1;
    let fx = repo(kind);
    if fx.is_skipped() {
        return;
    }
    let fx = fx
        .git(["checkout", "-q", "-b", "side"])
        .write("a/g", "side\n")
        .write("both", "side\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "side"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("a/g", "main\n")
        .write("both", "main\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "main"])
        .tick();
    let out = fx.git_output(["merge", "-q", "side"]);
    assert!(!out.status.success());
    let index = check_round_trip(&fx, kind, "conflicted");
    assert!(index.has_conflicts());
    let groups: Vec<_> = index.unmerged().map(|g| g[0].path().to_vec()).collect();
    assert_eq!(groups, [b"a/g".to_vec(), b"both".to_vec()]);
    assert_eq!(index.entries_for_path(b"both").len(), 2);
    index.verify_order().unwrap();

    // Resolving one path creates a REUC extension, which is kept.
    let fx = fx.write("a/g", "resolved\n").git(["add", "a/g"]);
    let index = check_round_trip(&fx, kind, "resolve undo");
    assert!(index.resolve_undo_raw().is_some());
}

/// Edits made by rgit produce the same file as the same edits made by git
/// (`update-index --cacheinfo` / `--force-remove`, which leave stat data
/// zero), including the cache tree invalidation.
#[test]
fn edits_match_update_index() {
    for kind in HashKind::ALL {
        let fx = repo(kind);
        if fx.is_skipped() {
            return;
        }
        let blob = ObjectId::from_hex(
            kind,
            String::from_utf8(fx.git_stdout(["hash-object", "-w", "a/g"]))
                .unwrap()
                .trim()
                .as_bytes(),
        )
        .unwrap();
        let path = index_file(&fx);
        let mut index = read(&path, kind);
        assert!(index.cache_tree().unwrap().root.is_valid());

        index
            .add_path("a/b/new", Mode::EXECUTABLE, blob, AddOptions::default())
            .unwrap();
        assert!(index.remove(b"c/h", Stage::MERGED).is_some());
        // Replace an existing entry: stat data is reset like --cacheinfo does.
        index
            .add(
                Entry::new("a.b/c", Stage::MERGED, Mode::FILE, blob),
                AddOptions::default(),
            )
            .unwrap();
        // "top" becomes a directory.
        let err = index
            .add_path("top/x", Mode::FILE, blob, AddOptions::default())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "'top/x' appears as both a file and as a directory"
        );
        let replace = AddOptions {
            ok_to_replace: true,
            ..AddOptions::default()
        };
        index.add_path("top/x", Mode::FILE, blob, replace).unwrap();
        let ours = index.to_bytes(&WriteOptions::default());

        let _fx = fx
            .git([
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100755,{blob},a/b/new"),
            ])
            .git(["update-index", "--force-remove", "c/h"])
            .git([
                "update-index",
                "--cacheinfo",
                &format!("100644,{blob},a.b/c"),
            ])
            .git([
                "update-index",
                "--add",
                "--replace",
                "--cacheinfo",
                &format!("100644,{blob},top/x"),
            ]);
        let theirs = std::fs::read(&path).unwrap();
        let theirs_index = read(&path, kind);
        assert_same_text(
            "edits",
            &ls_files_debug(&theirs_index),
            &ls_files_debug(&index),
        );
        assert_eq!(theirs_index.cache_tree(), index.cache_tree(), "{kind}");
        assert!(theirs == ours, "{kind}: edited index differs from git's");
    }
}

/// Unknown optional extensions are ignored (and dropped on write), unknown
/// required ones are refused, with git's messages.
#[test]
fn extensions_against_git() {
    let kind = HashKind::Sha1;
    let fx = repo(kind);
    if fx.is_skipped() {
        return;
    }
    let path = index_file(&fx);
    let original = std::fs::read(&path).unwrap();
    let body = &original[..original.len() - 20];
    let with_ext = |sig: &[u8; 4]| {
        let mut data = body.to_vec();
        data.extend_from_slice(sig);
        data.extend_from_slice(&3u32.to_be_bytes());
        data.extend_from_slice(b"xyz");
        let sum = rgit_hash::hash(kind, &data).unwrap();
        data.extend_from_slice(sum.as_bytes());
        data
    };

    std::fs::write(&path, with_ext(b"ABCD")).unwrap();
    let out = fx.git_output(["ls-files"]);
    let index = read(&path, kind);
    assert_eq!(index.ignored_extensions(), [*b"ABCD"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!(
            "{}\n",
            rgit_index::messages::ignoring_extension(&index.ignored_extensions()[0])
        )
    );
    assert!(index.to_bytes(&WriteOptions::default()) == original);

    std::fs::write(&path, with_ext(b"abcd")).unwrap();
    let out = fx.git_output(["ls-files"]);
    let err = Index::read_file(&path, kind, &ReadOptions::default()).unwrap_err();
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        err.report(".git/index").join("\n") + "\n"
    );
}

/// git's messages for a bad header.
#[test]
fn header_errors_against_git() {
    let kind = HashKind::Sha1;
    let fx = repo(kind);
    if fx.is_skipped() {
        return;
    }
    let path = index_file(&fx);
    let original = std::fs::read(&path).unwrap();
    let mut bad_sig = original.clone();
    bad_sig[3] = b'X';
    let mut bad_version = original.clone();
    bad_version[7] = 7;
    for (label, data) in [
        ("signature", bad_sig),
        ("version", bad_version),
        ("small", original[..25].to_vec()),
    ] {
        std::fs::write(&path, &data).unwrap();
        let out = fx.git_output(["ls-files"]);
        let err = Index::read_file(&path, kind, &ReadOptions::default()).unwrap_err();
        assert_same_text(
            label,
            &String::from_utf8_lossy(&out.stderr),
            &(err.report(".git/index").join("\n") + "\n"),
        );
    }
}

/// Optional: read a real-world index given in `RGIT_INDEX_TEST_FILE`
/// (for example a copy of the `upstream/git` checkout's index) and compare
/// with git. The file is copied, never modified.
#[test]
fn external_index() {
    let Some(src) = std::env::var_os("RGIT_INDEX_TEST_FILE") else {
        return;
    };
    let kind = match std::env::var("RGIT_INDEX_TEST_HASH").as_deref() {
        Ok("sha256") => HashKind::Sha256,
        _ => HashKind::Sha1,
    };
    let fx = Fixture::new().git(["init", "-q", &format!("--object-format={}", kind.name())]);
    if fx.is_skipped() {
        return;
    }
    std::fs::copy(src, index_file(&fx)).unwrap();
    let index = check_round_trip(&fx, kind, "external index");
    eprintln!(
        "external index: {} entries, version {}",
        index.len(),
        index.version()
    );
}
