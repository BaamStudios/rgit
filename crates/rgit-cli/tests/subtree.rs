// SPDX-License-Identifier: MIT
//! `subtree` against the reference git: messages, the commits written by
//! `add`, `merge`, `pull` and `split --rejoin`, and the synthetic history
//! of `split` (identical commit ids), with repository state compared.
//!
//! Skipped when the reference git has no `subtree` (it is a contrib
//! command that not every installation ships).

use rgit_difftest::{Check, Fixture, Normalizer};

fn fixture() -> Option<Fixture> {
    let fx = Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"));
    if fx.is_skipped() || !fx.git_output(["subtree", "-h"]).status.success() {
        eprintln!("skipping: the reference git has no subtree command");
        return None;
    }
    Some(fx)
}

/// Commit a file named `path` (content: its name) in `repo`.
fn commit(fx: Fixture, repo: &str, path: &str) -> Fixture {
    fx.tick()
        .write(format!("{repo}/{path}"), format!("{path}\n"))
        .git(["-C", repo, "add", path])
        .git(["-C", repo, "commit", "-q", "-m", path])
}

/// `sub` (two commits) and `main` (one commit).
fn two_repos() -> Option<Fixture> {
    let fx = fixture()?
        .git(["init", "-q", "sub"])
        .git(["init", "-q", "main"]);
    let fx = commit(fx, "sub", "s1");
    let fx = commit(fx, "sub", "dir/s2");
    Some(commit(fx, "main", "m1"))
}

fn same(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["subtree"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .in_dir("main")
        .repo("main")
        // git's debug output names its cache directory.
        .normalize_stderr(Normalizer::drop_lines("^Using cachedir: "))
        .run();
}

fn same_output(fx: &mut Fixture, args: &[&str]) {
    let mut argv = vec!["subtree"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .in_dir("main")
        .repo("main")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn usage_and_option_errors() {
    let Some(fx) = two_repos() else { return };
    let mut fx = fx;
    for args in [
        &["-h"][..],
        &[],
        &["--bogus"],
        &["split", "-P"],
        &["split"],
        &["-P", "x"],
        &["bogus", "-P", "x"],
        &["add", "-P", "x", "--annotate=foo", "HEAD"],
        &["merge", "-P", "x", "--rejoin", "HEAD"],
        &["split", "-P", "x", "--squash"],
        &["split", "-P", "x", "-m", "msg"],
        &["push", "-P", "x", "--no-squash", "a", "b"],
        &["pull", "-P", "x", "-b", "br", "a", "b"],
        &["add", "-P", "m1", "HEAD"],
        &["add", "-P", "x"],
        &["add", "-P", "x", "a", "b", "c"],
        &["add", "-P", "x", "nonexistent"],
        &["add", "-P", "x", "../sub", "bad..ref"],
        &["merge", "-P", "x", "HEAD"],
        &["split", "-P", "x"],
        &["split", "-P", "m1"],
        &["split", "-P", "x", "a", "b", "c"],
        &["split", "-P", "x", "nonexistent"],
        &["pull", "-P", "m1", "../sub"],
        &["push", "-P", "m1", "../sub"],
        &["push", "-P", "x", "../sub", "a:b:c"],
        &["push", "-P", "x", "../sub", "nonexistent:b"],
        &["push", "-P", "x", "../sub", "HEAD:b"],
    ] {
        same_output(&mut fx, args);
    }
}

#[test]
fn help_outside_a_repository() {
    let Some(mut fx) = fixture() else { return };
    fx.compare(["subtree", "-h"]).checks(Check::OUTPUT).run();
    fx.compare(["subtree"]).checks(Check::OUTPUT).run();
}

#[test]
fn add_merge_pull_and_split() {
    let Some(mut fx) = two_repos() else { return };
    same(&mut fx, &["add", "-P", "lib", "../sub", "main"]);
    let fx = commit(fx, "main", "lib/m-lib");
    let fx = commit(fx, "main", "m2");
    let mut fx2 = commit(fx, "sub", "s3");
    same(&mut fx2, &["pull", "-P", "lib", "../sub", "main"]);
    same(&mut fx2, &["split", "-P", "lib"]);
    same(
        &mut fx2,
        &["split", "-P", "lib", "--annotate=(split) ", "-b", "spl"],
    );
    same(&mut fx2, &["split", "-P", "lib", "-b", "spl"]);
    same(&mut fx2, &["split", "-P", "lib", "--rejoin", "-b", "spl2"]);
    let mut fx = commit(fx2, "main", "lib/after-rejoin");
    same(&mut fx, &["split", "-d", "-P", "lib"]);
    same(&mut fx, &["split", "-P", "lib", "--ignore-joins"]);
    same(&mut fx, &["split", "-q", "-P", "lib", "-b", "main"]);
    let mut fx = commit(fx, "sub", "s4");
    fx = fx.git(["-C", "main", "fetch", "-q", "../sub", "main"]);
    same(
        &mut fx,
        &["merge", "-P", "lib", "-m", "custom merge", "FETCH_HEAD"],
    );
    same(
        &mut fx,
        &["add", "-P", "lib2", "-m", "custom add", "FETCH_HEAD~1"],
    );
}

#[test]
fn squash_add_merge_and_rejoin() {
    let Some(mut fx) = two_repos() else { return };
    same(&mut fx, &["add", "-P", "lib", "--squash", "../sub", "main"]);
    let fx = commit(fx, "main", "lib/m-lib");
    let mut fx = commit(fx, "sub", "s3");
    same(
        &mut fx,
        &["pull", "-d", "-P", "lib", "--squash", "../sub", "main"],
    );
    fx = fx.git(["-C", "main", "fetch", "-q", "../sub", "main"]);
    same(&mut fx, &["merge", "-P", "lib", "--squash", "FETCH_HEAD~2"]);
    same(&mut fx, &["merge", "-P", "lib", "--squash", "FETCH_HEAD~2"]);
    same(&mut fx, &["merge", "-P", "lib", "--squash", "FETCH_HEAD"]);
    same(
        &mut fx,
        &[
            "split", "-P", "lib", "--rejoin", "--squash", "-m", "rejoined",
        ],
    );
    same(&mut fx, &["split", "-P", "lib"]);
    same(
        &mut fx,
        &[
            "add",
            "-P",
            "other",
            "--squash",
            "-m",
            "squashed add",
            "HEAD",
        ],
    );
}

#[test]
fn split_from_scratch_with_rejoin_and_push() {
    let Some(fx) = fixture() else { return };
    let fx = fx
        .git(["init", "-q", "main"])
        .git(["init", "-q", "--bare", "pub.git"]);
    let fx = commit(fx, "main", "top");
    let fx = commit(fx, "main", "pkg/a");
    let fx = commit(fx, "main", "other");
    let mut fx = commit(fx, "main", "pkg/b");
    same(&mut fx, &["split", "-P", "pkg", "--rejoin", "HEAD~1"]);
    let mut fx = commit(fx, "main", "pkg/c");
    // The pushed loose objects are compressed differently: compare the
    // published refs instead of the files.
    fx.compare([
        "subtree",
        "push",
        "-P",
        "pkg",
        "../pub.git",
        "+HEAD:published",
    ])
    .in_dir("main")
    .repo("main")
    .skip(Check::Worktree)
    .run();
    fx.compare(["for-each-ref"])
        .in_dir("pub.git")
        .repo("pub.git")
        .run();
    same(
        &mut fx,
        &["split", "-P", "pkg", "--onto=HEAD~3", "-b", "onto"],
    );
}

#[test]
fn split_keeps_merges_and_annotates_messages() {
    let Some(fx) = fixture() else { return };
    let fx = fx.git(["init", "-q", "main"]);
    let fx = commit(fx, "main", "pkg/a");
    let fx = fx.git(["-C", "main", "checkout", "-q", "-b", "side"]);
    let fx = commit(fx, "main", "pkg/side");
    let fx = commit(fx, "main", "outside");
    let fx = fx.git(["-C", "main", "checkout", "-q", "main"]);
    let fx = commit(fx, "main", "pkg/main");
    let fx = fx.tick().git([
        "-C",
        "main",
        "merge",
        "-q",
        "--no-ff",
        "-m",
        "merge side",
        "side",
    ]);
    // An unrelated history with the same subtree.
    let fx = fx
        .git(["-C", "main", "checkout", "-q", "--orphan", "other"])
        .git(["-C", "main", "rm", "-q", "-rf", "."])
        .write("main/pkg/a", "pkg/a\n")
        .write("main/pkg/side", "pkg/side\n")
        .write("main/pkg/main", "pkg/main\n")
        .git(["-C", "main", "add", "pkg"])
        .git(["-C", "main", "commit", "-q", "-m", "no trailing newline"])
        .git(["-C", "main", "checkout", "-q", "main"])
        .tick()
        .git([
            "-C",
            "main",
            "merge",
            "-q",
            "--no-ff",
            "--allow-unrelated-histories",
            "-m",
            "merge other",
            "other",
        ]);
    let mut fx = commit(fx, "main", "pkg/last");
    same(
        &mut fx,
        &[
            "split",
            "-d",
            "-P",
            "pkg/",
            "--annotate=[pkg] ",
            "-b",
            "out",
        ],
    );
}
