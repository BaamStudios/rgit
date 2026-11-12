// SPDX-License-Identifier: MIT

//! Patch headers (`diff --git`, modes, index lines, quoting, binary files,
//! symlinks) compared with `git diff` between two commits.

mod support;

use rgit_diff::{MODE_SYMLINK, PatchFile, PatchOptions, write_file_patch};
use rgit_hash::{HashKind, hash_object};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use support::{Scratch, have_git};

#[derive(Clone)]
struct Entry {
    mode: u32,
    data: Vec<u8>,
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", dir)
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[cfg(unix)]
fn write_tree(dir: &Path, tree: &BTreeMap<Vec<u8>, Entry>) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    for (path, e) in tree {
        let p = dir.join(std::ffi::OsStr::from_bytes(path));
        if e.mode == MODE_SYMLINK {
            std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(&e.data), &p).unwrap();
        } else {
            std::fs::write(&p, &e.data).unwrap();
            let perm = if e.mode == 0o100755 { 0o755 } else { 0o644 };
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(perm)).unwrap();
        }
    }
}

#[cfg(unix)]
fn clear_tree(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != ".git" {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
}

fn side<'a>(path: &'a [u8], e: Option<&'a Entry>) -> Option<PatchFile<'a>> {
    e.map(|e| PatchFile {
        path,
        mode: e.mode,
        oid: hash_object(HashKind::Sha1, "blob", &e.data).unwrap(),
        data: &e.data,
    })
}

fn expected(old: &BTreeMap<Vec<u8>, Entry>, new: &BTreeMap<Vec<u8>, Entry>) -> Vec<u8> {
    let mut paths: Vec<&Vec<u8>> = old.keys().chain(new.keys()).collect();
    paths.sort();
    paths.dedup();
    let opts = PatchOptions::default();
    let mut out = Vec::new();
    for path in paths {
        let (o, n) = (side(path, old.get(path)), side(path, new.get(path)));
        let type_change = matches!((&o, &n), (Some(a), Some(b))
            if (a.mode == MODE_SYMLINK) != (b.mode == MODE_SYMLINK));
        if type_change {
            write_file_patch(&mut out, o.as_ref(), None, &opts);
            write_file_patch(&mut out, None, n.as_ref(), &opts);
        } else if o.as_ref().map(|f| (f.mode, f.oid)) != n.as_ref().map(|f| (f.mode, f.oid)) {
            write_file_patch(&mut out, o.as_ref(), n.as_ref(), &opts);
        }
    }
    out
}

fn e(mode: u32, data: &[u8]) -> Entry {
    Entry {
        mode,
        data: data.to_vec(),
    }
}

#[cfg(unix)]
#[test]
fn headers_match_git() {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let s = Scratch::new("patch-headers");
    let dir = &s.0;
    git(dir, &["init", "-q", "-b", "main"]);
    let text: Vec<u8> = (0..30)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .into();
    let mut text2 = text.clone();
    text2.extend_from_slice(b"more\n");
    let mut old = BTreeMap::new();
    old.insert(
        b"modified.c".to_vec(),
        e(0o100644, b"int main()\n{\n  return 0;\n}\n"),
    );
    old.insert(b"deleted".to_vec(), e(0o100644, &text));
    old.insert(b"deleted-empty".to_vec(), e(0o100644, b""));
    old.insert(b"chmod-only".to_vec(), e(0o100644, &text));
    old.insert(b"chmod-and-edit".to_vec(), e(0o100755, &text));
    old.insert(b"bin".to_vec(), e(0o100644, b"\0\x01\x02"));
    old.insert(b"link".to_vec(), e(MODE_SYMLINK, b"target-a"));
    old.insert(b"to-link".to_vec(), e(0o100644, b"was a file\n"));
    old.insert(b"with space".to_vec(), e(0o100644, b"a\n"));
    old.insert("t\u{e4}st".as_bytes().to_vec(), e(0o100644, b"a\n"));
    old.insert(b"tab\there".to_vec(), e(0o100644, b"a\n"));
    old.insert(b"noeol".to_vec(), e(0o100644, b"a\nb"));

    let mut new = old.clone();
    new.insert(
        b"modified.c".to_vec(),
        e(0o100644, b"int main()\n{\n  return 1;\n}\n"),
    );
    new.remove(b"deleted".as_slice());
    new.remove(b"deleted-empty".as_slice());
    new.insert(b"added".to_vec(), e(0o100644, &text2));
    new.insert(b"added-empty".to_vec(), e(0o100644, b""));
    new.insert(b"added-exec".to_vec(), e(0o100755, b"#!/bin/sh\n"));
    new.insert(b"chmod-only".to_vec(), e(0o100755, &text));
    new.insert(b"chmod-and-edit".to_vec(), e(0o100644, &text2));
    new.insert(b"bin".to_vec(), e(0o100644, b"\0\x01\x03"));
    new.insert(b"bin-new".to_vec(), e(0o100644, b"x\0y"));
    new.insert(b"link".to_vec(), e(MODE_SYMLINK, b"target-b"));
    new.insert(b"to-link".to_vec(), e(MODE_SYMLINK, b"modified.c"));
    new.insert(b"with space".to_vec(), e(0o100644, b"b\n"));
    new.insert("t\u{e4}st".as_bytes().to_vec(), e(0o100644, b"b\n"));
    new.insert(b"tab\there".to_vec(), e(0o100644, b"b\n"));
    new.insert(b"noeol".to_vec(), e(0o100644, b"a\nc"));

    write_tree(dir, &old);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "one"]);
    clear_tree(dir);
    write_tree(dir, &new);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "two"]);
    let want = git(
        dir,
        &["diff", "--no-renames", "--no-color", "HEAD~1", "HEAD"],
    );
    let got = expected(&old, &new);
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want)
    );
}
