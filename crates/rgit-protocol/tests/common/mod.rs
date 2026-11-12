// SPDX-License-Identifier: MIT
//! Helpers for tests that run the reference `git` binary as a black box.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The reference git: `$RGIT_REFERENCE_GIT` or `git` from `PATH`; `None`
/// (test skipped) if it cannot be run, unless `RGIT_REQUIRE_REFERENCE=1`.
pub fn reference_git() -> Option<PathBuf> {
    let git = std::env::var_os("RGIT_REFERENCE_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("git"));
    let ok = Command::new(&git)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        Some(git)
    } else if std::env::var_os("RGIT_REQUIRE_REFERENCE").is_some_and(|v| v == "1") {
        panic!("reference git not found");
    } else {
        eprintln!("skipping: reference git not found");
        None
    }
}

/// A `git` command with a deterministic, isolated environment.
pub fn git(git: &Path, home: &Path) -> Command {
    let mut c = Command::new(git);
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1112911993 -0700")
        .env("GIT_COMMITTER_DATE", "1112911993 -0700")
        .env("LC_ALL", "C")
        .env("TZ", "UTC");
    c
}

/// Run and require success.
pub fn run(mut c: Command) -> Output {
    let out = c.output().expect("spawn git");
    assert!(
        out.status.success(),
        "{c:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

/// Create `<dir>/repo` with a commit on `main`, a branch `side` and an
/// annotated tag `v1`.
pub fn make_repo(git_bin: &Path, dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    let mut c = git(git_bin, dir);
    c.args(["init", "-q", "-b", "main"]).arg(&repo);
    run(c);
    for args in [
        &["commit", "-q", "--allow-empty", "-m", "one"][..],
        &["tag", "-a", "v1", "-m", "v1"],
        &["branch", "side"],
    ] {
        let mut c = git(git_bin, dir);
        c.arg("-C").arg(&repo).args(args);
        run(c);
    }
    repo
}
