// SPDX-License-Identifier: MIT
//! Differential tests of small behaviours fixed in #767 and #794: the
//! `FETCH_HEAD` URL of a one-letter `.git` path, `clone` of a remote whose
//! `HEAD` is unborn while it has other branches, the dangling-symref
//! warnings of `log` / `diff`, and which duplicate `worktree prune` drops.

use rgit_difftest::{Check, Fixture};

const OUTPUT: [Check; 3] = [Check::ExitCode, Check::Stdout, Check::Stderr];
const CONFIG: [Check; 5] = [
    Check::ExitCode,
    Check::Stdout,
    Check::Stderr,
    Check::Refs,
    Check::GitFiles,
];

fn fixture() -> Option<Fixture> {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .git(["commit", "-q", "--allow-empty", "-m", "a"]);
    (!fx.is_skipped()).then_some(fx)
}

/// `b.git` keeps its suffix in `FETCH_HEAD` (only one character would
/// remain), `ab.git` loses it.
#[test]
fn fetch_head_url() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .git(["init", "-q", "--bare", "b.git"])
        .git(["init", "-q", "--bare", "ab.git"])
        .git(["push", "-q", "b.git", "main"])
        .git(["push", "-q", "ab.git", "main"]);
    for url in ["b.git", "b.git/", "ab.git"] {
        fx.compare(["fetch", "-q", url, "main"])
            .checks(CONFIG)
            .run();
    }
}

/// The remote has a branch but its `HEAD` names an unborn one: the clone
/// still configures that branch to track the remote. Without the remote's
/// `HEAD` (protocol v0) the clone keeps its initial branch and says so;
/// `--single-branch` has nothing to fetch.
#[test]
fn clone_unborn_remote_head() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .git(["init", "-q", "--bare", "-b", "master", "e.git"])
        .git(["push", "-q", "e.git", "main:refs/heads/other"]);
    let cases: [(&str, &[&str]); 4] = [
        (
            "c2",
            &["-c", "protocol.version=2", "clone", "-q", "e.git", "c2"],
        ),
        (
            "c0",
            &["-c", "protocol.version=0", "clone", "-q", "e.git", "c0"],
        ),
        ("cs", &["clone", "-q", "--single-branch", "e.git", "cs"]),
        ("cd", &["clone", "-q", "--depth=1", "e.git", "cd"]),
    ];
    for (repo, args) in cases {
        fx.compare(args).repo(repo).checks(CONFIG).run();
    }
}

/// A name that is neither a revision nor a path is looked up once more
/// for the error: git warns about a dangling symref twice.
#[test]
fn dangling_symref_warnings() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx.git(["symbolic-ref", "refs/heads/dang", "refs/heads/nowhere"]);
    let cases: [&[&str]; 6] = [
        &["log", "--oneline", "dang"],
        &["log", "--oneline", "dang", "--"],
        &["log", "--oneline", "dang..main"],
        &["rev-list", "dang"],
        &["show", "-s", "dang"],
        &["diff", "dang"],
    ];
    for args in cases {
        fx.compare(args).checks(OUTPUT).run();
    }
}

/// Of two worktrees with the same path the one whose id sorts first
/// stays, whatever the order of the directory entries (`zz` is created
/// first).
#[test]
fn prune_duplicate_worktree() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .git(["config", "worktree.useRelativePaths", "true"])
        .git(["worktree", "add", "-q", "--detach", "zz"])
        .git(["worktree", "add", "-q", "--detach", "aa"])
        .write(".git/worktrees/aa/gitdir", "../../../zz/.git\n");
    fx.compare(["worktree", "prune", "-v"])
        .checks([Check::ExitCode, Check::Stderr])
        .run();
}

/// `:` alone and the empty index path are no paths on disk: `log :` is
/// an ambiguous argument (t4208), `:0:` does not exist anywhere.
#[test]
fn empty_index_path_arguments() {
    let Some(mut fx) = fixture() else { return };
    for args in [
        &["log", ":"][..],
        &["rev-list", ":"],
        &["show", ":"],
        &["log", ":0:"],
        &["rev-parse", "--verify", ":"],
    ] {
        fx.compare(args).checks(OUTPUT).run();
    }
}

/// A pattern refspec does not write through a symbolic reference here:
/// `refs/remotes/*:refs/remotes/*` updates `origin/main` once and leaves
/// `origin/HEAD` out (t5535), with and without `--atomic`.
#[test]
fn pattern_refspec_skips_symbolic_destinations() {
    let Some(fx) = fixture() else { return };
    let mut fx = fx
        .git(["clone", "-q", ".", "src"])
        .git(["clone", "-q", "src", "dst"])
        .git(["clone", "-q", "src", "dst2"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "b"])
        .git(["-C", "src", "fetch", "-q"]);
    let state = [
        Check::ExitCode,
        Check::Stdout,
        Check::Stderr,
        Check::Refs,
        Check::Reflogs,
    ];
    fx.compare(["fetch", "../src", "refs/remotes/*:refs/remotes/*"])
        .in_dir("dst")
        .repo("dst")
        .checks(state)
        .run();
    fx.compare([
        "fetch",
        "--atomic",
        "../src",
        "refs/remotes/*:refs/remotes/*",
    ])
    .in_dir("dst2")
    .repo("dst2")
    .checks(state)
    .run();
}
