// SPDX-License-Identifier: MIT
//! `fetch` against the reference git: summary on stderr, exit code, refs,
//! reflogs, `FETCH_HEAD` and objects, over a local path, `file://`,
//! `git daemon` and smart HTTP (`git http-backend` behind a small CGI
//! server in this file). The packet trace check compares the number of
//! `have` lines.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::{server_path, start_daemon, start_http};

const CHECKS: [Check; 7] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::GitFiles,
    Check::Objects,
];

fn commit(fx: Fixture, dir: &str, file: &str, msg: &str) -> Fixture {
    fx.write(format!("{dir}/{file}"), format!("{msg}\n"))
        .git(["-C", dir, "add", file])
        .git(["-C", dir, "commit", "-q", "-m", msg])
        .tick()
}

/// `srv` with branches and tags, `empty` (a repository with `origin`),
/// `cl` (a clone of `srv`), then new history in `srv`: a new commit and
/// tags, a deleted and a new branch, a rewritten branch.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "srv"]);
    let fx = commit(fx, "srv", "a", "one")
        .git(["-C", "srv", "tag", "v1"])
        .git(["-C", "srv", "tag", "-a", "-m", "ann", "v1a"])
        .git([
            "-C",
            "srv",
            "checkout",
            "-q",
            "-b",
            "a-very-long-branch-name-here",
        ]);
    let fx = commit(fx, "srv", "b", "two")
        .git(["-C", "srv", "checkout", "-q", "main"])
        .git(["-C", "srv", "branch", "side"])
        .git(["init", "-q", "empty"])
        .git(["-C", "empty", "remote", "add", "origin", "../srv"])
        .git(["clone", "-q", "srv", "cl"]);
    let fx = commit(fx, "srv", "c", "three")
        .git(["-C", "srv", "tag", "v2"])
        .git([
            "-C",
            "srv",
            "branch",
            "-q",
            "-D",
            "a-very-long-branch-name-here",
        ])
        .git(["-C", "srv", "branch", "newb"])
        .git(["-C", "srv", "checkout", "-q", "side"]);
    commit(fx, "srv", "s", "side")
        .git(["-C", "srv", "commit", "-q", "--amend", "-m", "side-amended"])
        .git(["-C", "srv", "tag", "-a", "-m", "x", "v3"])
        .git(["-C", "srv", "checkout", "-q", "main"])
}

fn compare(fx: &mut Fixture, repo: &str, args: &[&str]) {
    let path = server_path(fx);
    fx.compare(args)
        .in_dir(repo)
        .repo(repo)
        .env("PATH", path)
        .checks(CHECKS)
        .run();
}

#[test]
fn into_empty_repository() {
    let cases: &[&[&str]] = &[
        &["fetch"],
        &["fetch", "origin"],
        &["fetch", "../srv", "main"],
        &["fetch", "origin", "main:refs/heads/x", "tag", "v1"],
        &["fetch", "--dry-run", "origin"],
        &["fetch", "-q"],
        &["fetch", "--no-tags"],
        &["fetch", "--tags"],
        &["fetch", "-v", "origin", "HEAD"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        compare(&mut fx, "empty", args);
    }
}

#[test]
fn updates() {
    let cases: &[&[&str]] = &[
        &["fetch"],
        &["fetch", "--prune"],
        &["fetch", "--prune", "--dry-run"],
        &["fetch", "-v"],
        &[
            "fetch",
            "-f",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        &[
            "fetch",
            "origin",
            "main:refs/remotes/origin/main",
            "side:refs/remotes/origin/side",
        ],
        &["fetch", "-q", "origin", "side:refs/remotes/origin/side"],
        &["fetch", "origin", "side"],
        &["fetch", "origin", "main:main"],
        &["fetch", "origin", "nope"],
        &["fetch", "nosuchremote"],
        &["fetch", "origin", "refs/heads/*:refs/x/*"],
        &["fetch", ".", "refs/remotes/origin/main"],
        &["fetch", "--tags"],
    ];
    for args in cases {
        let mut fx = fixture();
        if fx.is_skipped() {
            return;
        }
        compare(&mut fx, "cl", args);
    }
}

#[test]
fn configuration() {
    let mut fx = fixture()
        .git(["-C", "cl", "checkout", "-q", "-b", "work", "origin/main"])
        .git(["-C", "cl", "config", "branch.work.remote", "origin"])
        .git(["-C", "cl", "config", "branch.work.merge", "refs/heads/main"]);
    if fx.is_skipped() {
        return;
    }
    compare(&mut fx, "cl", &["fetch"]);

    let mut fx = fixture().git(["-C", "cl", "config", "--unset", "remote.origin.fetch"]);
    compare(&mut fx, "cl", &["fetch"]);

    let mut fx = fixture().git(["-C", "srv", "tag", "-f", "v1", "main"]);
    compare(&mut fx, "cl", &["fetch", "--tags"]);
    let mut fx = fixture().git(["-C", "srv", "tag", "-f", "v1", "main"]);
    compare(&mut fx, "cl", &["fetch", "--tags", "--force"]);
}

#[test]
fn column_width() {
    let mut fx = fixture()
        .git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbb"])
        .git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbbb"]);
    if fx.is_skipped() {
        return;
    }
    compare(&mut fx, "cl", &["fetch", "-v"]);
    let mut fx = fixture().git(["-C", "srv", "branch", "bbbbbbbbbbbbbbbbbbbbbbb"]);
    let path = server_path(&fx);
    fx.compare(["fetch"])
        .in_dir("cl")
        .repo("cl")
        .env("PATH", path)
        .env("COLUMNS", "200")
        .checks(CHECKS)
        .run();
}

/// The packet trace of one fetch, as lines.
fn trace(fx: &Fixture, bin: &Path, dir: &Path, out: &Path) -> String {
    let status = Command::new(bin)
        .arg("fetch")
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE_PACKET", out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read_to_string(out).unwrap()
}

#[test]
fn have_lines_match() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "big"]);
    if fx.is_skipped() {
        return;
    }
    for i in 0..40 {
        fx = commit(fx, "big", "f", &format!("c{i}"));
    }
    fx = fx.git(["clone", "-q", "big", "local"]);
    for i in 0..30 {
        fx = commit(fx, "local", "l", &format!("l{i}"));
    }
    for i in 40..50 {
        fx = commit(fx, "big", "f", &format!("c{i}"));
    }
    let work = fx.work_dir();
    let copy = |name: &str| {
        let dst = work.join(name);
        let st = Command::new("cp")
            .args(["-a"])
            .arg(work.join("local"))
            .arg(&dst)
            .status()
            .unwrap();
        assert!(st.success());
        dst
    };
    let (g, r) = (copy("g"), copy("r"));
    let count = |t: &str| t.lines().filter(|l| l.contains("fetch> have ")).count();
    let git_haves = count(&trace(&fx, fx.reference_git(), &g, &work.join("g.trace")));
    let rgit = PathBuf::from(env!("CARGO_BIN_EXE_rgit"));
    let rgit_haves = count(&trace(&fx, &rgit, &r, &work.join("r.trace")));
    assert!(git_haves > 0);
    assert_eq!(git_haves, rgit_haves);
}

#[test]
fn git_daemon() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let Some((_daemon, base)) = start_daemon(&fx) else {
        return;
    };
    let url = format!("{base}/srv");
    let mut fx = fx.git(["-C", "empty", "remote", "set-url", "origin", &url]);
    compare(&mut fx, "empty", &["fetch"]);
    compare(
        &mut fx,
        "cl",
        &["fetch", &url, "+refs/heads/*:refs/remotes/origin/*"],
    );
}

#[test]
fn smart_http() {
    let fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let base = start_http(&fx);
    let url = format!("{base}/srv");
    let mut fx = fx.git(["-C", "empty", "remote", "set-url", "origin", &url]);
    compare(&mut fx, "empty", &["fetch"]);
    compare(
        &mut fx,
        "cl",
        &[
            "fetch",
            "--prune",
            &url,
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
}
