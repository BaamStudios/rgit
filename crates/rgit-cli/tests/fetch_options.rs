// SPDX-License-Identifier: MIT
//! Differential tests of the `fetch` options beyond the M2 core (#164,
//! #328): several remotes (`--all`, `--multiple`, groups, `fetch.all`),
//! `--prune-tags`, `--refmap`, `--atomic`, `--set-upstream`, `--porcelain`,
//! `fetch.output=compact`, `followRemoteHEAD`, worktree checkouts and
//! destinations that are not full ref names.

use rgit_difftest::{Check, Fixture};

mod common;
use common::servers::server_path;

const CHECKS: [Check; 6] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::Reflogs,
    Check::GitFiles,
];

fn commit(fx: Fixture, dir: &str, msg: &str) -> Fixture {
    fx.git(["-C", dir, "commit", "-q", "--allow-empty", "-m", msg])
        .tick()
}

/// `one` and `two` (a clone of `one` with more history), `c` a clone of
/// `one` with both and a broken remote.
fn fixture() -> Fixture {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main", "one"]);
    let fx = commit(fx, "one", "one")
        .git(["-C", "one", "tag", "t1"])
        .git(["-C", "one", "tag", "-a", "-m", "a", "anno"])
        .git(["-C", "one", "branch", "side"])
        .git(["clone", "-q", "one", "two"]);
    let fx = commit(fx, "two", "two")
        .git(["clone", "-q", "one", "c"])
        .git(["-C", "c", "remote", "add", "two", "../two"])
        .git(["-C", "c", "remote", "add", "bad", "../nonexistent"]);
    commit(fx, "one", "three")
        .git(["-C", "one", "branch", "-D", "side"])
        .git(["-C", "one", "tag", "-d", "t1"])
}

#[test]
fn fetch_several_remotes_same_as_git() {
    let mut fx = fixture()
        .git(["-C", "c", "config", "remotes.grp", "origin two"])
        .git(["-C", "c", "config", "remote.bad.skipFetchAll", "false"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["fetch", "--all"],
        &["fetch", "-q", "--all"],
        &["fetch", "--all", "origin"],
        &["fetch", "--multiple", "origin", "two"],
        &["fetch", "--multiple", "four"],
        &["fetch", "grp"],
        &["fetch", "-v", "grp"],
        &["fetch", "grp", "main"],
        &["-c", "fetch.all=true", "fetch"],
        &[
            "-c",
            "fetch.parallel=2",
            "fetch",
            "--multiple",
            "origin",
            "bad",
        ],
        &["fetch", "--porcelain", "--multiple", "origin", "two"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("c")
            .repo("c")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}

#[test]
fn fetch_refspec_options_same_as_git() {
    let mut fx = fixture().git(["-C", "c", "tag", "localtag"]).git([
        "-C",
        "c",
        "update-ref",
        "refs/hidden/x",
        "HEAD",
    ]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["fetch", "--dry-run", "--prune", "--prune-tags", "origin"],
        &["fetch", "--dry-run", "--prune", "--tags", "origin"],
        &["fetch", "--dry-run", "--prune-tags", "origin"],
        &[
            "-c",
            "fetch.pruneTags=true",
            "fetch",
            "--dry-run",
            "-p",
            "origin",
        ],
        &["fetch", "--dry-run", "--refmap=", "origin", "main"],
        &[
            "fetch",
            "--dry-run",
            "--refmap=refs/heads/*:refs/x/*",
            "origin",
            "main",
        ],
        &[
            "fetch",
            "--dry-run",
            "--no-write-fetch-head",
            "origin",
            "main",
        ],
        &["fetch", "--no-write-fetch-head", "origin", "main"],
        &["fetch", "origin", "anno:five"],
        &["fetch", "origin", "main:foo"],
        &["fetch", "--porcelain", "origin", "+refs/heads/*:refs/p/*"],
        &["-c", "fetch.output=compact", "fetch", "origin"],
        &["-c", "fetch.output=garbage", "fetch", "origin"],
        &["-c", "fetch.output", "fetch", "origin"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("c")
            .repo("c")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}

#[test]
fn fetch_atomic_and_upstream_same_as_git() {
    let mut fx = fixture()
        .git(["-C", "c", "update-ref", "refs/remotes/origin/side", "HEAD"])
        .git(["-C", "one", "branch", "side", "HEAD~1"])
        .git(["-C", "c", "commit", "-q", "--allow-empty", "-m", "local"])
        .git(["-C", "c", "update-ref", "refs/remotes/origin/side", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &[
            "fetch",
            "--atomic",
            "origin",
            "refs/heads/*:refs/remotes/origin/*",
        ],
        &["fetch", "--atomic", "--prune", "origin"],
        &["fetch", "--set-upstream", "two", "main"],
        &["fetch", "--set-upstream", "two"],
        &["fetch", "--set-upstream", "../two"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("c")
            .repo("c")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}

#[test]
fn fetch_remote_head_and_worktrees_same_as_git() {
    let mut fx = fixture()
        .git(["-C", "c", "update-ref", "refs/remotes/origin/other", "HEAD"])
        .git([
            "-C",
            "c",
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/other",
        ])
        .git(["-C", "c", "worktree", "add", "-q", "-b", "wt", "../wt"]);
    if fx.is_skipped() {
        return;
    }
    let cases: &[&[&str]] = &[
        &["-c", "fetch.followRemoteHEAD=warn", "fetch"],
        &[
            "-c",
            "remote.origin.followRemoteHEAD=warn-if-not-main",
            "fetch",
        ],
        &[
            "-c",
            "remote.origin.followRemoteHEAD=warn-if-not-x",
            "fetch",
            "-q",
        ],
        &["-c", "fetch.followRemoteHEAD=always", "fetch"],
        &["fetch", "origin", "+main:wt"],
        &["fetch", "-u", "origin", "+main:wt"],
    ];
    for argv in cases {
        let path = server_path(&fx);
        fx.compare(*argv)
            .in_dir("c")
            .repo("c")
            .env("PATH", path)
            .checks(CHECKS)
            .run();
    }
}
