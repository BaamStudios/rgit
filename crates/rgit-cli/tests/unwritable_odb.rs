// SPDX-License-Identifier: MIT
//! Commands that write objects into a read-only object database report the
//! problem with git's messages and exit codes (compared with the reference
//! git).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn run(bin: &Path, home: &Path, repo: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = Command::new(bin)
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The object directory and its fan-out directories.
fn object_dirs(repo: &Path) -> Vec<PathBuf> {
    let objects = repo.join(".git/objects");
    let mut dirs = vec![objects.clone()];
    for e in std::fs::read_dir(&objects).unwrap() {
        let p = e.unwrap().path();
        if p.file_name().unwrap().len() == 2 {
            dirs.push(p);
        }
    }
    dirs
}

fn set_writable(repo: &Path, writable: bool) {
    for d in object_dirs(repo) {
        let mode = if writable { 0o775 } else { 0o555 };
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// Runs `args` in a fresh repository (one commit of `file`, a changed
/// `file` staged, `file` changed again) with a read-only object database.
fn outcome(bin: &Path, args: &[&str]) -> Option<(Option<i32>, String)> {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let repo = dir.path().join("r");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let git = rgit_difftest::env::reference_git()?;
    for setup in [
        &["init", "-q"][..],
        &["add", "file"],
        &["commit", "-q", "-m", "one"],
    ] {
        if setup[0] == "add" {
            std::fs::write(repo.join("file"), "").unwrap();
        }
        assert_eq!(run(&git, &home, &repo, setup).0, Some(0));
    }
    std::fs::write(repo.join("file"), "\n").unwrap();
    assert_eq!(run(&git, &home, &repo, &["add", "file"]).0, Some(0));
    std::fs::write(repo.join("file"), "changed\n").unwrap();
    set_writable(&repo, false);
    // Permissions do not apply (e.g. running as root).
    let probe = repo.join(".git/objects/probe");
    if std::fs::write(&probe, "").is_ok() {
        let _ = std::fs::remove_file(&probe);
        set_writable(&repo, true);
        return None;
    }
    let result = run(bin, &home, &repo, args);
    set_writable(&repo, true);
    Some(result)
}

#[test]
fn messages_like_git() {
    let Some(git) = rgit_difftest::env::reference_git() else {
        return;
    };
    let rgit = Path::new(env!("CARGO_BIN_EXE_rgit"));
    for args in [
        &["write-tree"][..],
        &["commit", "-m", "two"],
        &["commit", "-q", "-a", "-m", "two"],
        &["update-index", "file"],
        &["add", "file"],
        &["hash-object", "-w", "file"],
    ] {
        let Some(want) = outcome(&git, args) else {
            return;
        };
        let got = outcome(rgit, args).unwrap();
        assert_eq!(want, got, "{args:?}");
    }
}
