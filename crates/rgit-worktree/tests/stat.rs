// SPDX-License-Identifier: MIT
//! Stat comparison and racy-git handling, checked against the reference
//! git: an index written by one side must be read by the other without
//! re-hashing, and racily clean entries must be caught.

#![cfg(unix)]

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rgit_difftest::Fixture;
use rgit_hash::{HashKind, hash_object};
use rgit_index::{AddOptions, Entry, Index, ReadOptions, Stage, Time, WriteOptions};
use rgit_worktree::stat::{
    Checker, StatOptions, Status, index_mtime, mode_from_metadata, smudge_racy_entries,
    stat_from_metadata,
};

fn set_mtime(path: &Path, t: SystemTime) {
    let f = File::options().write(true).open(path).unwrap();
    f.set_times(FileTimes::new().set_modified(t)).unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn read_index(dir: &Path) -> (Index, Option<Time>) {
    let path = dir.join(".git/index");
    let t = index_mtime(&path).unwrap();
    let index = Index::read_file(&path, HashKind::Sha1, &ReadOptions::default()).unwrap();
    (index, t)
}

/// The work tree column of `git status --porcelain` for every tracked
/// path, computed by rgit, in git's format (` M`, ` T`, ` D`).
fn rgit_status(dir: &Path, opts: StatOptions) -> (String, usize) {
    let (index, t) = read_index(dir);
    let checker = Checker::new(dir, HashKind::Sha1, opts, t);
    let mut out = String::new();
    for e in index.entries() {
        let code = match checker.check(e).unwrap() {
            Status::Unchanged | Status::StatOnly => continue,
            Status::Modified => 'M',
            Status::TypeChanged => 'T',
            Status::Deleted => 'D',
        };
        out.push_str(&format!(" {code} {}\n", String::from_utf8_lossy(e.path())));
    }
    (out, checker.files_hashed())
}

/// git's view without touching the index.
fn git_status(fx: &Fixture, extra: &[&str]) -> String {
    let mut args = extra.to_vec();
    args.extend_from_slice(&[
        "--no-optional-locks",
        "status",
        "--porcelain",
        "--untracked-files=no",
    ]);
    // Keep only the work tree column.
    String::from_utf8(fx.git_stdout(args))
        .unwrap()
        .lines()
        .filter(|l| l.as_bytes().get(1) != Some(&b' '))
        .map(|l| format!(" {}\n", &l[1..]))
        .collect()
}

/// Same-size modification within the second of `git add`: the stat data
/// still matches, but the entry is racily clean and the content check
/// finds the change. Repeated to catch timing luck.
#[test]
fn racy_modification_after_git_add() {
    let mut fx = Fixture::new().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let file = dir.join("infocom");
    for i in 0..20 {
        fx = fx.write("infocom", format!("frotz{}\n", i % 10)).git([
            "update-index",
            "--add",
            "infocom",
        ]);
        let before = mtime(&file);
        std::fs::write(&file, format!("xyzzy{}\n", i % 10)).unwrap();
        // Keep the old mtime: only the racy rule can see the change.
        set_mtime(&file, before);
        set_mtime(&dir.join(".git/index"), before);
        let (ours, _) = rgit_status(&dir, StatOptions::default());
        assert_eq!(ours, " M infocom\n", "iteration {i}");
        assert_eq!(ours, git_status(&fx, &[]), "iteration {i}");
    }
}

/// The 100-iteration loop of #36 without the reference git: rgit stages a
/// file, writes the index and the file changes in the same second.
#[test]
fn racy_loop_rgit_only() {
    let tmp = tempdir("racy-loop");
    std::fs::create_dir_all(tmp.join(".git")).unwrap();
    let file = tmp.join("cornerstone");
    for i in 0..100 {
        std::fs::write(&file, format!("abc{}\n", i % 10)).unwrap();
        let mut index = Index::new(HashKind::Sha1);
        let meta = std::fs::symlink_metadata(&file).unwrap();
        let data = std::fs::read(&file).unwrap();
        let mut e = Entry::new(
            "cornerstone",
            Stage::MERGED,
            mode_from_metadata(&meta, &StatOptions::default(), None).unwrap(),
            hash_object(HashKind::Sha1, "blob", &data).unwrap(),
        );
        e.stat = stat_from_metadata(&meta);
        index.add(e, AddOptions::default()).unwrap();
        std::fs::write(
            tmp.join(".git/index"),
            index.to_bytes(&WriteOptions::default()),
        )
        .unwrap();
        std::fs::write(&file, format!("xyz{}\n", i % 10)).unwrap();
        // Whatever the clock did, the file must not look older than the
        // index or the stat data must differ.
        let (ours, _) = rgit_status(&tmp, StatOptions::default());
        assert_eq!(ours, " M cornerstone\n", "iteration {i}");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// After git refreshed the index, rgit decides every entry from the stat
/// data alone; after rgit refreshed it, git's `diff-files` (which does not
/// refresh) sees no stat difference.
#[test]
fn refreshed_index_needs_no_hashing() {
    let mut fx = Fixture::new().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    for i in 0..20 {
        fx = fx.write(format!("d/f{i}"), format!("{i}\n"));
    }
    fx = fx
        .write_executable("run.sh", "#!/bin/sh\n")
        .git(["add", "."]);
    let dir = fx.work_dir();
    let old = SystemTime::now() - Duration::from_secs(100);
    for i in 0..20 {
        set_mtime(&dir.join(format!("d/f{i}")), old);
    }
    set_mtime(&dir.join("run.sh"), old);
    // git refreshes (stat data changed) and writes the index now.
    fx = fx.git(["update-index", "--refresh"]);
    let (ours, hashed) = rgit_status(&dir, StatOptions::default());
    assert_eq!(ours, "");
    assert_eq!(hashed, 0, "rgit re-hashed files git had refreshed");

    // Touch everything again; rgit refreshes and writes the index.
    let older = old - Duration::from_secs(100);
    for i in 0..20 {
        set_mtime(&dir.join(format!("d/f{i}")), older);
    }
    let (mut index, t) = read_index(&dir);
    let checker = Checker::new(&dir, HashKind::Sha1, StatOptions::default(), t);
    for pos in 0..index.len() {
        let e = index.entries()[pos].clone();
        let meta = std::fs::symlink_metadata(dir.join(String::from_utf8_lossy(e.path()).as_ref()))
            .unwrap();
        if checker.check(&e).unwrap() == Status::StatOnly {
            checker.refresh(index.entry_mut(pos).unwrap(), &meta);
        }
    }
    assert_eq!(checker.files_hashed(), 20);
    std::fs::write(
        dir.join(".git/index"),
        index.to_bytes(&WriteOptions::default()),
    )
    .unwrap();
    let raw = fx.git_stdout(["diff-files"]);
    assert_eq!(
        String::from_utf8_lossy(&raw),
        "",
        "git sees stat changes after rgit's refresh"
    );
}

/// racy-git.adoc, second problem: an entry modified in the second of the
/// index write must stay "changed" after a later index write moves the
/// index timestamp past it. rgit smudges it (size 0) and git agrees.
#[test]
fn smudged_entry_survives_later_index_write() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("foo", "aaa\n")
        .write("bar", "bbb\n")
        .git(["update-index", "--add", "foo", "bar"]);
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let foo = dir.join("foo");
    let t0 = mtime(&dir.join(".git/index"));
    std::fs::write(&foo, "zzz\n").unwrap();
    set_mtime(&foo, t0);
    set_mtime(&dir.join(".git/index"), t0);

    let (mut index, t) = read_index(&dir);
    let checker = Checker::new(&dir, HashKind::Sha1, StatOptions::default(), t);
    let n = smudge_racy_entries(&mut index, t.unwrap(), &checker).unwrap();
    assert_eq!(n, 1);
    assert_eq!(index.get(b"foo", Stage::MERGED).unwrap().stat.size, 0);
    assert_ne!(index.get(b"bar", Stage::MERGED).unwrap().stat.size, 0);
    let path = dir.join(".git/index");
    std::fs::write(&path, index.to_bytes(&WriteOptions::default())).unwrap();
    // The new index is much newer: foo is no longer racy by timestamp.
    set_mtime(&path, t0 + Duration::from_secs(60));
    assert_eq!(git_status(&fx, &[]), " M foo\n");
    assert_eq!(rgit_status(&dir, StatOptions::default()).0, " M foo\n");
}

/// File type, executable bit and the `core.*` switches, against git.
#[test]
fn types_modes_and_config() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("exec", "x\n")
        .write("typed", "t\n")
        .write("gone", "g\n")
        .write("dir", "d\n")
        .symlink("target", "link")
        .git(["add", "."]);
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let old = SystemTime::now() - Duration::from_secs(100);
    for f in ["exec", "typed", "gone", "dir"] {
        set_mtime(&dir.join(f), old);
    }
    let fx = fx.git(["update-index", "--refresh"]);
    std::fs::set_permissions(dir.join("exec"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::remove_file(dir.join("typed")).unwrap();
    std::os::unix::fs::symlink("exec", dir.join("typed")).unwrap();
    std::fs::remove_file(dir.join("gone")).unwrap();
    std::fs::remove_file(dir.join("dir")).unwrap();
    std::fs::create_dir(dir.join("dir")).unwrap();
    std::fs::write(dir.join("dir/inner"), "").unwrap();

    let (ours, _) = rgit_status(&dir, StatOptions::default());
    assert_eq!(ours, git_status(&fx, &[]));

    let no_mode = StatOptions {
        file_mode: false,
        ..StatOptions::default()
    };
    let (ours, _) = rgit_status(&dir, no_mode);
    assert_eq!(ours, git_status(&fx, &["-c", "core.fileMode=false"]));
    assert!(!ours.contains("exec"));

    // core.symlinks=false: a regular file holding the target stands in for
    // the link.
    std::fs::remove_file(dir.join("link")).unwrap();
    std::fs::write(dir.join("link"), "target").unwrap();
    let no_links = StatOptions {
        symlinks: false,
        ..StatOptions::default()
    };
    let (ours, _) = rgit_status(&dir, no_links);
    assert_eq!(ours, git_status(&fx, &["-c", "core.symlinks=false"]));
    assert!(!ours.contains("link"));
}

/// `core.trustCtime=false` and `core.checkStat=minimal` ignore changes
/// that only touch the inode change time or the inode.
#[test]
fn trust_ctime_and_check_stat() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("f", "same\n")
        .git(["add", "f"]);
    if fx.is_skipped() {
        return;
    }
    let dir = fx.work_dir();
    let f = dir.join("f");
    let old = SystemTime::now() - Duration::from_secs(100);
    set_mtime(&f, old);
    let fx = fx.git(["update-index", "--refresh"]);
    let (index, _) = read_index(&dir);
    let e = index.get(b"f", Stage::MERGED).unwrap().clone();
    // chmod changes only ctime; replacing the file changes the inode.
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    let meta = std::fs::symlink_metadata(&f).unwrap();
    let opts = |trust_ctime, minimal| StatOptions {
        trust_ctime,
        check_stat: if minimal {
            rgit_worktree::stat::CheckStat::Minimal
        } else {
            rgit_worktree::stat::CheckStat::Default
        },
        ..StatOptions::default()
    };
    let changed = |o: StatOptions| !rgit_worktree::stat::compare(&e, &meta, &o).is_empty();
    assert!(changed(opts(true, false)));
    assert!(!changed(opts(false, false)));
    let copy = dir.join("f.new");
    std::fs::copy(&f, &copy).unwrap();
    set_mtime(&copy, old);
    std::fs::rename(&copy, &f).unwrap();
    let meta = std::fs::symlink_metadata(&f).unwrap();
    let changed = |o: StatOptions| !rgit_worktree::stat::compare(&e, &meta, &o).is_empty();
    assert!(changed(opts(false, false)), "inode change");
    assert!(!changed(opts(false, true)), "minimal ignores the inode");

    // git agrees on what "unchanged" means: diff-files lists stat-dirty
    // entries.
    let dirty = |args: &[&str]| {
        let mut a = args.to_vec();
        a.push("diff-files");
        !fx.git_stdout(a).is_empty()
    };
    assert!(dirty(&["-c", "core.trustctime=false"]));
    assert!(!dirty(&[
        "-c",
        "core.trustctime=false",
        "-c",
        "core.checkstat=minimal"
    ]));
}

fn tempdir(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "rgit-worktree-{tag}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}
