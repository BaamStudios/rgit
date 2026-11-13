// SPDX-License-Identifier: MIT
//! Differential tests of `rgit status` (long, short and porcelain
//! formats) against the reference git. Rename detection is not
//! implemented yet, so git runs with `status.renames=false`.

use rgit_difftest::{Check, Fixture};

fn check(fx: &mut Fixture, args: &[&str]) {
    check_in(fx, ".", args);
}

fn check_in(fx: &mut Fixture, dir: &str, args: &[&str]) {
    let mut argv = vec!["-c", "status.renames=false", "status"];
    argv.extend_from_slice(args);
    fx.compare(&argv).in_dir(dir).checks(Check::OUTPUT).run();
}

/// The human formats.
const HUMAN: &[&[&str]] = &[
    &[],
    &["-uno"],
    &["-uall"],
    &["--no-ahead-behind"],
    &["-s"],
    &["-sb"],
    &["--short", "--branch", "--no-ahead-behind"],
    &["-s", "-z", "-b"],
    &["-s", "-uno"],
];

fn check_all(fx: &mut Fixture) {
    for args in MODES.iter().chain(HUMAN) {
        check(fx, args);
    }
    let mut argv = vec!["-c", "advice.statusHints=false", "status"];
    for extra in [None, Some("-uno")] {
        argv.truncate(3);
        argv.extend(extra);
        fx.compare(&argv).checks(Check::OUTPUT).run();
    }
}

const MODES: &[&[&str]] = &[
    &["--porcelain"],
    &["--porcelain=v2"],
    &["--porcelain", "-b"],
    &["--porcelain=v2", "--branch"],
    &["-z"],
    &["--porcelain=v2", "-z", "-b"],
    &["--porcelain", "-uno"],
    &["--porcelain", "--untracked-files=normal"],
    &["--porcelain", "-uall"],
    &["--porcelain=v2", "-uall", "-z"],
    &["--porcelain", "u"],
];

#[test]
fn unborn_branch() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .git(["add", "a"]);
    if fx.is_skipped() {
        return;
    }
    check_all(&mut fx);
}

#[test]
fn changes_untracked_and_ignored() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("d/x", "x\n")
        .write("sp ace", "s\n")
        .write("t", "t\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("a", "changed\n")
        .git(["rm", "-q", "--cached", "b"])
        .write("new", "n\n")
        .git(["add", "new"])
        .write("new", "n2\n")
        .remove("d/x")
        .write_executable("sp ace", "s\n")
        .remove("t")
        .symlink("a", "t")
        .write("u/v/f", "1\n")
        .write(".gitignore", "*.log\n")
        .write("z.log", "l\n")
        .write("ig/q.log", "q\n")
        .write("ita", "i\n")
        .git(["add", "-N", "ita"])
        .write("\u{fc}mlaut", "u\n")
        .write("q\"uote", "q\n");
    if fx.is_skipped() {
        return;
    }
    check_all(&mut fx);
}

#[test]
fn conflict_and_detached() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["checkout", "-q", "-b", "side"])
        .write("a", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "main\n")
        .git(["commit", "-q", "-a", "-m", "main"]);
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "-q", "side"]);
    check_all(&mut fx);
    let mut fx = fx
        .git(["reset", "-q", "--hard"])
        .git(["checkout", "-q", "--detach"]);
    check_all(&mut fx);
}

#[test]
fn subdirectory_relative_paths() {
    let mut fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("sub/s", "s\n")
        .write("sub/deep/d", "d\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .write("a", "changed\n")
        .write("sub/s", "changed\n")
        .write("sub/u", "u\n")
        .write("sub/ud/q", "q\n")
        .write("other/o", "o\n")
        .write("sp ace", "x\n")
        .git(["add", "sub/s"]);
    if fx.is_skipped() {
        return;
    }
    for args in HUMAN {
        check_in(&mut fx, "sub", args);
    }
    check_in(&mut fx, "sub", &["--porcelain"]);
    check_in(&mut fx, "sub", &["."]);
    check_in(&mut fx, "sub", &["-s", ".."]);
    check_in(&mut fx, "sub/deep", &["-s"]);
    let argv = ["-c", "status.relativePaths=false", "status"];
    fx.compare(argv).in_dir("sub").checks(Check::OUTPUT).run();
}

#[test]
fn upstream_tracking() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"])
        .git(["config", "remote.origin.url", "/nonexistent"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["config", "branch.main.remote", "origin"])
        .git(["config", "branch.main.merge", "refs/heads/main"]);
    if fx.is_skipped() {
        return;
    }
    let mut fx = fx;
    // Up to date, then ahead, behind, diverged.
    check_all(&mut fx);
    let mut fx = fx
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "three"]);
    check_all(&mut fx);
    let mut fx = fx
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "four"]);
    check_all(&mut fx);
    let mut fx = fx.git(["reset", "-q", "--hard", "HEAD~3"]);
    check_all(&mut fx);
    let mut fx = fx
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "five"]);
    check_all(&mut fx);
    fx.compare(["-c", "status.aheadBehind=false", "status"])
        .checks(Check::OUTPUT)
        .run();
    fx.compare([
        "-c",
        "status.aheadBehind=false",
        "status",
        "--porcelain=v2",
        "-b",
    ])
    .checks(Check::OUTPUT)
    .run();
    // Upstream is a local branch, then gone.
    let mut fx = fx
        .git(["branch", "-q", "base", "HEAD~1"])
        .git(["config", "branch.main.remote", "."])
        .git(["config", "branch.main.merge", "refs/heads/base"]);
    check_all(&mut fx);
    let mut fx = fx.git(["config", "branch.main.merge", "refs/heads/nope"]);
    check_all(&mut fx);
    // Unborn branch with an upstream.
    let mut fx = fx
        .git(["checkout", "-q", "--orphan", "orphan"])
        .git(["config", "branch.orphan.remote", "origin"])
        .git(["config", "branch.orphan.merge", "refs/heads/main"]);
    check_all(&mut fx);
}

#[test]
fn detached_at_and_from() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["commit", "-q", "--allow-empty", "-m", "two"])
        .git(["tag", "v1"])
        .git(["tag", "-a", "-m", "t", "v2"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD"]);
    if fx.is_skipped() {
        return;
    }
    let steps: &[&[&str]] = &[
        &["checkout", "-q", "--detach"],
        &["checkout", "-q", "v1"],
        &["checkout", "-q", "v2"],
        &["checkout", "-q", "origin/main"],
        &["commit", "-q", "--allow-empty", "-m", "three"],
        &["checkout", "-q", "main~1"],
        &["checkout", "-q", "--detach", "main"],
        &["update-ref", "--no-deref", "HEAD", "HEAD~1"],
    ];
    let mut fx = fx;
    for step in steps {
        fx = fx.tick().git(*step);
        check(&mut fx, &[]);
        check(&mut fx, &["-sb"]);
    }
}

#[test]
fn merge_in_progress() {
    let fx = Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("a", "a\n")
        .write("b", "b\n")
        .write("c", "c\n")
        .write("d", "d\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .git(["checkout", "-q", "-b", "side"])
        .write("a", "side\n")
        .write("d", "side\n")
        .git(["commit", "-q", "-a", "-m", "side"])
        .git(["checkout", "-q", "main"])
        .write("a", "main\n")
        .git(["rm", "-q", "d"])
        .git(["commit", "-q", "-a", "-m", "main"]);
    if fx.is_skipped() {
        return;
    }
    let _ = fx.git_output(["merge", "-q", "side"]);
    let mut fx = fx
        .write("b", "changed\n")
        .write("c", "staged\n")
        .git(["add", "c"])
        .write("u", "u\n");
    check_all(&mut fx);
    let mut fx = fx.write("a", "resolved\n").git(["add", "a"]);
    check_all(&mut fx);
    let mut fx = fx.git(["rm", "-q", "d"]);
    check_all(&mut fx);
    // Unmerged entries without a merge in progress.
    let fx = fx.git(["merge", "--abort"]);
    let _ = fx.git_output(["merge", "-q", "side"]);
    let mut fx = fx.remove(".git/MERGE_HEAD");
    check_all(&mut fx);
}
